//! Pairs the real Matter driver with a dimmable light simulated by rs-matter on localhost,
//! then drives it through the hub like the dashboard does.

use std::collections::HashMap;
use std::net::{Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::Duration;

use embassy_futures::select::select3;
use hestia_core::{Command, DeviceId, Origin, Value};
use hestia_hub::{Hub, spawn_driver};
use hestia_matter::{MatterConfig, MatterDriver, pairing_channel};
use rs_matter::crypto::{Crypto, default_crypto};
use rs_matter::dm::clusters::app::level_control::{
    self, AttributeDefaults, LevelControlHandler, LevelControlHooks as _, OptionsBitmap,
    test::TestLevelControlDeviceLogic,
};
use rs_matter::dm::clusters::app::on_off::{
    self, OnOffHandler, OnOffHooks as _, test::TestOnOffDeviceLogic,
};
use rs_matter::dm::clusters::desc::{self, ClusterHandler as _};
use rs_matter::dm::devices::DEV_TYPE_DIMMABLE_LIGHT;
use rs_matter::dm::devices::test::{DAC_PRIVKEY, TEST_DEV_ATT, TEST_DEV_COMM, TEST_DEV_DET};
use rs_matter::dm::endpoints::EthSysHandlerBuilder;
use rs_matter::dm::networks::SysNetifs;
use rs_matter::dm::networks::eth::EthNetwork;
use rs_matter::dm::{Async, Dataver, Endpoint, Node};
use rs_matter::im::{EthInteractionModelState, InteractionModel};
use rs_matter::persist::DummyKvBlobStore;
use rs_matter::respond::DefaultResponder;
use rs_matter::sc::pase::MAX_COMM_WINDOW_TIMEOUT_SECS;
use rs_matter::tlv::Nullable;
use rs_matter::transport::exchange::MatterBuffers;
use rs_matter::transport::network::NoNetwork;
use rs_matter::{Matter, clusters, devices, root_endpoint};

/// Manual pairing code of rs-matter's test commissioning data (passcode 20202021).
const TEST_CODE: &str = "3497-011-2332";

const LIGHT: Node<'static> = Node {
    endpoints: &[
        root_endpoint!(eth),
        Endpoint::new(
            1,
            devices!(DEV_TYPE_DIMMABLE_LIGHT),
            clusters!(
                desc::DescHandler::CLUSTER,
                TestOnOffDeviceLogic::CLUSTER,
                TestLevelControlDeviceLogic::CLUSTER
            ),
        ),
    ],
};

/// A commissionable dimmable light listening on `[::]:port`, on its own thread.
fn start_fake_light(port: u16) {
    std::thread::Builder::new()
        .name("fake-light".to_owned())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let matter = Matter::new(&TEST_DEV_DET, TEST_DEV_COMM, &TEST_DEV_ATT, port);
            let buffers: MatterBuffers = MatterBuffers::new();
            let state: EthInteractionModelState =
                EthInteractionModelState::new(EthNetwork::new_default());
            let kv = matter.kv(DummyKvBlobStore);
            let crypto = default_crypto(rand::rng(), DAC_PRIVKEY);
            let mut rand = crypto.rand().unwrap();
            let on_off = OnOffHandler::new(
                Dataver::new_rand(&mut rand),
                1,
                TestOnOffDeviceLogic::new(false),
            );
            let level = LevelControlHandler::new(
                Dataver::new_rand(&mut rand),
                1,
                TestLevelControlDeviceLogic::new(),
                AttributeDefaults {
                    on_level: Nullable::some(42),
                    options: OptionsBitmap::EXECUTE_IF_OFF,
                    on_off_transition_time: 0,
                    on_transition_time: Nullable::none(),
                    off_transition_time: Nullable::none(),
                    default_move_rate: Nullable::none(),
                },
            );
            on_off.init(Some(&level));
            level.init(Some(&on_off));
            let data_model = (
                LIGHT,
                EthSysHandlerBuilder::new()
                    .netif_diag(&SysNetifs)
                    .build(rand)
                    .chain(
                        |endpoint, cluster| {
                            endpoint == 1 && cluster == desc::DescHandler::CLUSTER.id
                        },
                        Async(desc::DescHandler::new(Dataver::new_rand(&mut rand)).adapt()),
                    )
                    .chain(
                        |endpoint, cluster| {
                            endpoint == 1 && cluster == TestLevelControlDeviceLogic::CLUSTER.id
                        },
                        level_control::HandlerAsyncAdaptor(&level),
                    )
                    .chain(
                        |endpoint, cluster| {
                            endpoint == 1 && cluster == TestOnOffDeviceLogic::CLUSTER.id
                        },
                        on_off::HandlerAsyncAdaptor(&on_off),
                    ),
            );
            let im = InteractionModel::new(&matter, &crypto, &buffers, data_model, &kv, &state);
            let responder = DefaultResponder::new(&im);
            matter
                .open_basic_comm_window(MAX_COMM_WINDOW_TIMEOUT_SECS, &crypto, &())
                .unwrap();
            let socket =
                async_io::Async::<UdpSocket>::bind(SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)))
                    .unwrap();
            let _ = futures_lite::future::block_on(select3(
                matter.run(&crypto, &socket, &socket, NoNetwork),
                responder.run::<4, 4>(),
                im.run(),
            ));
        })
        .unwrap();
}

fn free_udp_port() -> u16 {
    UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn wait_for(hub: &Hub, id: &DeviceId, point: &str, expected: &Value) {
    for _ in 0..100 {
        if hub.value(id, point).as_ref() == Some(expected) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!(
        "{point} never became {expected:?} (last: {:?})",
        hub.value(id, point)
    );
}

async fn set(hub: &Hub, id: &DeviceId, point: &str, value: Value) {
    let command = Command {
        device: id.clone(),
        point: point.to_owned(),
        value,
    };
    hub.command(command, Origin::Ui).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pairs_then_controls_a_light() {
    let port = free_udp_port();
    start_fake_light(port);
    let data_dir = std::env::temp_dir().join(format!("hestia-matter-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);

    let hub = Hub::new(HashMap::new());
    let (pairing, requests) = pairing_channel();
    let driver =
        MatterDriver::new(MatterConfig::default(), data_dir.clone()).with_pairing(&requests);
    spawn_driver(&hub, "matter", Arc::new(driver));

    let light = SocketAddr::from((Ipv6Addr::LOCALHOST, port));
    let paired = pairing
        .pair(TEST_CODE.to_owned(), Some(light))
        .await
        .expect("pairing");
    let id: DeviceId = paired.id.parse().unwrap();
    let device = hub.device(&id).expect("device declared in the hub");
    let points: Vec<&str> = device
        .points
        .iter()
        .map(|point| point.spec.key.as_str())
        .collect();
    assert_eq!(
        points,
        ["on", "brightness"],
        "a dimmable light without colour"
    );
    wait_for(&hub, &id, "on", &Value::Bool(false)).await;

    set(&hub, &id, "on", Value::Bool(true)).await;
    wait_for(&hub, &id, "on", &Value::Bool(true)).await;
    set(&hub, &id, "brightness", Value::Number(40.0)).await;
    wait_for(&hub, &id, "brightness", &Value::Number(40.0)).await;
    set(&hub, &id, "brightness", Value::Number(0.0)).await;
    wait_for(&hub, &id, "on", &Value::Bool(false)).await;
    assert!(
        hub.command(
            Command {
                device: id.clone(),
                point: "color".to_owned(),
                value: Value::Color(hestia_core::Rgb { r: 255, g: 0, b: 0 }),
            },
            Origin::Ui,
        )
        .await
        .is_err(),
        "no colour point on this light"
    );

    let stored = std::fs::read_to_string(data_dir.join("matter.json")).unwrap();
    assert!(stored.contains("\"root_key\""), "fabric saved");
    assert!(stored.contains(&format!(
        "\"node_id\": {}",
        u64::from_str_radix(id.native(), 16).unwrap()
    )));

    pairing
        .remove(id.native().to_owned())
        .await
        .expect("removal");
    let stored = std::fs::read_to_string(data_dir.join("matter.json")).unwrap();
    assert!(stored.contains("\"devices\": []"));
    let _ = std::fs::remove_dir_all(&data_dir);
}
