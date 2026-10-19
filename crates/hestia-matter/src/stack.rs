//! The Matter stack, on its own thread: rs-matter is single-threaded (`!Send`) and brings its
//! own async I/O. The driver hands it jobs through [`Stack`] and awaits the answers.

use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6, UdpSocket};
use std::num::NonZeroU8;
use std::time::Duration;

use embassy_futures::select::{Either, Either3, select, select3};
use mdns_sd::ServiceDaemon;
use rs_matter::Matter;
use rs_matter::cert::r#gen::VALID_FOREVER;
use rs_matter::cert::{MAX_CERT_TLV_AND_ASN1_LEN, MAX_CERT_TLV_LEN};
use rs_matter::crypto::{
    CanonAeadKey, CanonPkcSecretKey, Crypto, SecretKey as _, SigningSecretKey as _, default_crypto,
};
use rs_matter::dm::clusters::decl::color_control::{
    self as color_control, ColorControlClient as _,
};
use rs_matter::dm::clusters::decl::level_control::{
    self as level_control, LevelControlClient as _,
};
use rs_matter::dm::clusters::decl::on_off::OnOffClient as _;
use rs_matter::dm::clusters::decl::operational_credentials::{
    NodeOperationalCertStatusEnum, OperationalCredentialsClient as _,
};
use rs_matter::dm::devices::test::{DAC_PRIVKEY, TEST_DEV_ATT, TEST_DEV_COMM, TEST_DEV_DET};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::im::client::ImClient as _;
use rs_matter::im::{AttrPath, ReportDataResp};
use rs_matter::onboard::cac::RcacGenerator;
use rs_matter::onboard::noc::NocGenerator;
use rs_matter::onboard::{CommissionOptions, Commissioner};
use rs_matter::pairing::qr::QrPayload;
use rs_matter::tlv::{FromTLV, Nullable};
use rs_matter::transport::exchange::Exchange;
use rs_matter::transport::network::mdns::CommissionableFilter;
use rs_matter::transport::network::{Address, NoNetwork};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::light::{
    self, Action, CAPABILITY_HUE_SATURATION, CAPABILITY_TEMPERATURE, CAPABILITY_XY, LightState,
};
use crate::mdns::{self, AddressBook, mapped};
use crate::store::{FabricKeys, Features, Hex, PairedDevice};

/// rs-matter keeps whole messages on the stack.
const THREAD_STACK: usize = 16 * 1024 * 1024;
/// Vendor id written in the fabric: the test vendor, as chip-tool and other open controllers do.
const VENDOR_ID: u16 = 0xfff1;
/// Where Matter devices listen unless they announce otherwise.
pub const MATTER_PORT: u16 = 5540;
const BASIC_INFORMATION: u32 = 0x0028;
const VENDOR_NAME: u32 = 0x0001;
const PRODUCT_NAME: u32 = 0x0003;
const ROOT_ENDPOINT: u16 = 0;
/// Reads and commands that take longer have lost a message on the way.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(20);
/// Pairing waits for the device to join its fabric and announce itself.
const PAIRING_TIMEOUT: Duration = Duration::from_secs(150);
const BROWSE_TIMEOUT_MS: u32 = 15_000;
const FAIL_SAFE_S: u16 = 120;

/// A device on the fabric, and where it answered last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Target {
    pub node_id: u64,
    pub endpoint: u16,
    pub address: Option<SocketAddr>,
}

/// What a pairing code tells: the device's passcode and how to find it.
#[derive(Debug, Clone)]
pub struct SetupCode {
    passcode: u32,
    filter: CommissionableFilter,
}

impl SetupCode {
    /// Accepts the 11 or 21 digit manual code (dashes and spaces ignored) or the `MT:` QR text.
    ///
    /// # Errors
    /// Fails with a message for the household when the code is malformed.
    pub fn parse(code: &str) -> Result<Self, String> {
        let code = code.trim();
        let invalid = |_| format!("« {code} » n'est pas un code d'appairage Matter valide");
        if code.starts_with("MT:") {
            let mut buffer = [0_u8; 128];
            let payload = QrPayload::parse(code, &mut buffer).map_err(invalid)?;
            return Ok(Self {
                passcode: payload.passcode(),
                filter: payload.commissionable_filter(),
            });
        }
        let payload = QrPayload::<()>::parse_pairing_code(code).map_err(invalid)?;
        Ok(Self {
            passcode: payload.passcode(),
            filter: payload.commissionable_filter(),
        })
    }
}

pub(crate) type Reply<T> = oneshot::Sender<Result<T, String>>;

/// A result with the address the device answered from.
#[derive(Debug)]
pub(crate) struct Answer<T> {
    pub value: T,
    pub address: Option<SocketAddr>,
}

enum Job {
    Pair {
        code: SetupCode,
        address: Option<SocketAddr>,
        node_id: u64,
        reply: Reply<PairedDevice>,
    },
    Read {
        target: Target,
        attributes: Vec<(u32, u32)>,
        reply: Reply<Answer<LightState>>,
    },
    Invoke {
        target: Target,
        action: Action,
        reply: Reply<Answer<()>>,
    },
    Unpair {
        target: Target,
        reply: Reply<()>,
    },
}

/// Handle on the Matter thread. Dropping it stops the thread.
pub(crate) struct Stack {
    jobs: mpsc::Sender<Job>,
}

impl Stack {
    /// Starts the thread on `fabric`, or on a new fabric when there is none yet. Returns the
    /// fabric actually used, to be saved when it is new.
    ///
    /// # Errors
    /// Fails when the fabric, the UDP socket or mDNS cannot be set up.
    pub async fn start(fabric: Option<FabricKeys>) -> anyhow::Result<(Self, FabricKeys)> {
        Self::start_on(fabric, SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0))).await
    }

    pub(crate) async fn start_on(
        fabric: Option<FabricKeys>,
        bind: SocketAddr,
    ) -> anyhow::Result<(Self, FabricKeys)> {
        let (jobs, receiver) = mpsc::channel(16);
        let (ready, started) = oneshot::channel();
        std::thread::Builder::new()
            .name("matter".to_owned())
            .stack_size(THREAD_STACK)
            .spawn(move || run(fabric, bind, receiver, ready))?;
        let keys = started
            .await
            .map_err(|_| anyhow::anyhow!("Matter thread stopped during start-up"))?
            .map_err(|error| anyhow::anyhow!("cannot start Matter: {error}"))?;
        Ok((Self { jobs }, keys))
    }

    pub async fn pair(
        &self,
        code: SetupCode,
        address: Option<SocketAddr>,
        node_id: u64,
    ) -> Result<PairedDevice, String> {
        self.ask(|reply| Job::Pair {
            code,
            address,
            node_id,
            reply,
        })
        .await
    }

    pub async fn read(
        &self,
        target: Target,
        attributes: Vec<(u32, u32)>,
    ) -> Result<Answer<LightState>, String> {
        self.ask(|reply| Job::Read {
            target,
            attributes,
            reply,
        })
        .await
    }

    pub async fn invoke(&self, target: Target, action: Action) -> Result<Answer<()>, String> {
        self.ask(|reply| Job::Invoke {
            target,
            action,
            reply,
        })
        .await
    }

    pub async fn unpair(&self, target: Target) -> Result<(), String> {
        self.ask(|reply| Job::Unpair { target, reply }).await
    }

    async fn ask<T>(&self, job: impl FnOnce(Reply<T>) -> Job) -> Result<T, String> {
        let (reply, answer) = oneshot::channel();
        self.jobs
            .send(job(reply))
            .await
            .map_err(|_| "Matter stopped".to_owned())?;
        answer.await.map_err(|_| "Matter stopped".to_owned())?
    }
}

fn run(
    fabric: Option<FabricKeys>,
    bind: SocketAddr,
    jobs: mpsc::Receiver<Job>,
    ready: oneshot::Sender<Result<FabricKeys, String>>,
) {
    let crypto = default_crypto(rand::rng(), DAC_PRIVKEY);
    let matter = Matter::new(&TEST_DEV_DET, TEST_DEV_COMM, &TEST_DEV_ATT, 0);
    let prepared = (|| {
        let keys = match fabric {
            Some(keys) => keys,
            None => create_fabric(&crypto).map_err(|error| error.to_string())?,
        };
        let fab_idx = install(&matter, &crypto, &keys).map_err(|error| error.to_string())?;
        let socket = bind_socket(bind).map_err(|error| error.to_string())?;
        let daemon = ServiceDaemon::new().map_err(|error| error.to_string())?;
        Ok::<_, String>((keys, fab_idx, socket, daemon))
    })();
    let (keys, fab_idx, socket, daemon) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let _ = ready.send(Ok(keys.clone()));
    let book = AddressBook::default();
    let worker = Worker {
        matter: &matter,
        crypto: &crypto,
        fab_idx,
        keys: &keys,
        book: &book,
    };
    let outcome = futures_lite::future::block_on(select3(
        matter.run(&crypto, &socket, &socket, NoNetwork),
        mdns::serve(&matter, &daemon, &book),
        worker.serve(jobs),
    ));
    match outcome {
        Either3::First(Err(error)) => warn!(%error, "Matter transport stopped"),
        Either3::First(Ok(())) | Either3::Second(()) | Either3::Third(()) => {
            debug!("Matter thread stopping");
        }
    }
    let _ = daemon.shutdown();
}

/// A fabric of our own: a root certificate signing Hestia's certificate, then each device's.
fn create_fabric<C: Crypto>(crypto: C) -> Result<FabricKeys, Error> {
    let fabric_id = rand::random::<u64>().max(1);
    // Operational node ids stop at 0xFFFF_FFEF_FFFF_FFFF.
    let node_id = (rand::random::<u64>() >> 4).max(1);
    let mut root_buffer = vec![0_u8; MAX_CERT_TLV_AND_ASN1_LEN];
    let mut root_generator = RcacGenerator::new(&mut root_buffer);
    let (root_key, root_certificate) =
        root_generator.generate(&crypto, fabric_id, VALID_FOREVER)?;
    let root_certificate = root_certificate.to_vec();

    let key = crypto.generate_secret_key()?;
    let mut csr_buffer = [0_u8; 256];
    let csr = key.csr(&mut csr_buffer)?;
    let mut canon_key = CanonPkcSecretKey::new();
    key.write_canon(&mut canon_key)?;

    let mut noc_buffer = vec![0_u8; MAX_CERT_TLV_AND_ASN1_LEN];
    let mut generator = NocGenerator::create(
        root_key.reference(),
        &root_certificate,
        &[],
        &mut noc_buffer,
    )?;
    let certificate = generator
        .generate(&crypto, csr, node_id, &[], VALID_FOREVER)?
        .to_vec();
    info!(
        fabric = format!("{fabric_id:016x}"),
        "Matter fabric created"
    );
    Ok(FabricKeys {
        fabric_id,
        node_id,
        root_certificate: Hex(root_certificate),
        root_key: Hex(root_key.access().to_vec()),
        certificate: Hex(certificate),
        key: Hex(canon_key.access().to_vec()),
        ipk: Hex(rand::random::<[u8; 16]>().to_vec()),
    })
}

fn install<C: Crypto>(
    matter: &Matter<'_>,
    crypto: C,
    keys: &FabricKeys,
) -> Result<NonZeroU8, Error> {
    let mut key = CanonPkcSecretKey::new();
    key.try_load_from_slice(&keys.key.0)?;
    let mut ipk = CanonAeadKey::new();
    ipk.try_load_from_slice(&keys.ipk.0)?;
    matter.with_state(|state| {
        state
            .fabrics
            .add(
                crypto,
                key.reference(),
                &keys.root_certificate.0,
                &keys.certificate.0,
                &[],
                Some(ipk.reference()),
                VENDOR_ID,
                keys.node_id,
            )
            .map(|fabric| fabric.fab_idx())
    })
}

/// IPv6 socket that also carries IPv4, as IPv4-mapped addresses.
fn bind_socket(bind: SocketAddr) -> std::io::Result<async_io::Async<UdpSocket>> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_only_v6(false)?;
    let bind = match bind {
        SocketAddr::V4(v4) => {
            SocketAddr::V6(SocketAddrV6::new(v4.ip().to_ipv6_mapped(), v4.port(), 0, 0))
        }
        v6 @ SocketAddr::V6(_) => v6,
    };
    socket.bind(&bind.into())?;
    async_io::Async::new_nonblocking(UdpSocket::from(socket))
}

struct Worker<'a, C: Crypto> {
    matter: &'a Matter<'a>,
    crypto: &'a C,
    fab_idx: NonZeroU8,
    keys: &'a FabricKeys,
    book: &'a AddressBook,
}

impl<C: Crypto> Worker<'_, C> {
    /// Runs jobs one at a time until the driver drops its [`Stack`].
    async fn serve(&self, mut jobs: mpsc::Receiver<Job>) {
        while let Some(job) = jobs.recv().await {
            match job {
                Job::Pair {
                    code,
                    address,
                    node_id,
                    reply,
                } => {
                    let result = within(PAIRING_TIMEOUT, self.pair(&code, address, node_id)).await;
                    if let Err(error) = &result {
                        warn!(%error, "Matter pairing failed");
                        self.book.borrow_mut().remove(&node_id);
                    }
                    let _ = reply.send(result);
                }
                Job::Read {
                    target,
                    attributes,
                    reply,
                } => {
                    let result = within(EXCHANGE_TIMEOUT, self.read(&target, &attributes)).await;
                    let _ = reply.send(self.settle(&target, result));
                }
                Job::Invoke {
                    target,
                    action,
                    reply,
                } => {
                    let result = within(EXCHANGE_TIMEOUT, self.invoke(&target, action)).await;
                    let _ = reply.send(self.settle(&target, result));
                }
                Job::Unpair { target, reply } => {
                    let result = within(EXCHANGE_TIMEOUT, self.unpair(&target)).await;
                    self.book.borrow_mut().remove(&target.node_id);
                    let _ = reply.send(result);
                }
            }
        }
    }

    async fn pair(
        &self,
        code: &SetupCode,
        address: Option<SocketAddr>,
        node_id: u64,
    ) -> Result<PairedDevice, Error> {
        let peer = match address {
            Some(address) => Address::Udp(SocketAddr::new(mapped(address.ip()), address.port())),
            None => {
                self.matter
                    .transport()
                    .browse_commissionable(&code.filter, &[], BROWSE_TIMEOUT_MS)
                    .await?
                    .0
            }
        };
        info!(%peer, node = format!("{node_id:016x}"), "Matter pairing");
        let mut root_key = CanonPkcSecretKey::new();
        root_key.try_load_from_slice(&self.keys.root_key.0)?;
        let mut noc_buffer = vec![0_u8; MAX_CERT_TLV_AND_ASN1_LEN];
        let mut generator = NocGenerator::create(
            root_key.reference(),
            &self.keys.root_certificate.0,
            &[],
            &mut noc_buffer,
        )?;
        let mut scratch = vec![0_u8; MAX_CERT_TLV_LEN];
        let mut commissioner = Commissioner::new(
            self.matter,
            self.crypto,
            self.fab_idx,
            &mut generator,
            &mut scratch,
        );
        // rs-matter cannot check device attestation yet: the device is trusted because the
        // household typed its code.
        let options = CommissionOptions {
            fail_safe_secs: FAIL_SAFE_S,
            allow_test_attestation: true,
        };
        let result = commissioner
            .commission(peer, code.passcode, &options, node_id, VALID_FOREVER)
            .await?;
        if let Address::Udp(address) = peer {
            self.book.borrow_mut().insert(node_id, address);
        }
        commissioner.complete_via_case(&result).await?;
        info!(
            node = format!("{node_id:016x}"),
            "Matter device joined the fabric"
        );
        let mut device = self.probe(node_id).await?;
        device.address = self.book.borrow().get(&node_id).copied();
        Ok(device)
    }

    /// Reads who the device is and what its light can do.
    async fn probe(&self, node_id: u64) -> Result<PairedDevice, Error> {
        let paths = [
            path(Some(ROOT_ENDPOINT), BASIC_INFORMATION, VENDOR_NAME),
            path(Some(ROOT_ENDPOINT), BASIC_INFORMATION, PRODUCT_NAME),
            path(None, light::ON_OFF, light::ON_OFF_ON),
            path(None, light::LEVEL_CONTROL, light::LEVEL_CURRENT),
            path(None, light::COLOR_CONTROL, light::COLOR_CAPABILITIES),
            path(None, light::COLOR_CONTROL, light::COLOR_MIREDS_MIN),
            path(None, light::COLOR_CONTROL, light::COLOR_MIREDS_MAX),
        ];
        let mut vendor = String::new();
        let mut product = String::new();
        let mut lights: Vec<u16> = Vec::new();
        let mut dimmable: Vec<u16> = Vec::new();
        let mut capabilities: Vec<(u16, u16)> = Vec::new();
        let mut min_mireds: Vec<(u16, u16)> = Vec::new();
        let mut max_mireds: Vec<(u16, u16)> = Vec::new();
        self.read_paths(node_id, &paths, |resp| {
            for (_, name) in resp.attrs::<&str>(BASIC_INFORMATION, VENDOR_NAME) {
                vendor = name.map(str::to_owned).unwrap_or_default();
            }
            for (_, name) in resp.attrs::<&str>(BASIC_INFORMATION, PRODUCT_NAME) {
                product = name.map(str::to_owned).unwrap_or_default();
            }
            lights.extend(endpoints(
                resp.attrs::<bool>(light::ON_OFF, light::ON_OFF_ON),
            ));
            dimmable.extend(endpoints(
                resp.attrs::<Nullable<u8>>(light::LEVEL_CONTROL, light::LEVEL_CURRENT),
            ));
            capabilities.extend(values(
                resp.attrs::<u16>(light::COLOR_CONTROL, light::COLOR_CAPABILITIES),
            ));
            min_mireds.extend(values(
                resp.attrs::<u16>(light::COLOR_CONTROL, light::COLOR_MIREDS_MIN),
            ));
            max_mireds.extend(values(
                resp.attrs::<u16>(light::COLOR_CONTROL, light::COLOR_MIREDS_MAX),
            ));
        })
        .await?;
        // The light sits on the first application endpoint with OnOff.
        let endpoint = lights
            .iter()
            .copied()
            .filter(|endpoint| *endpoint != ROOT_ENDPOINT)
            .min()
            .or_else(|| lights.first().copied())
            .ok_or(ErrorCode::NotFound)?;
        let on = |list: &[(u16, u16)]| {
            list.iter()
                .find(|(found, _)| *found == endpoint)
                .map(|(_, value)| *value)
        };
        let capability = on(&capabilities).unwrap_or(0);
        let mireds = match (on(&min_mireds), on(&max_mireds)) {
            (Some(min), Some(max)) if capability & CAPABILITY_TEMPERATURE != 0 && min < max => {
                Some((min, max))
            }
            _ => None,
        };
        Ok(PairedDevice {
            node_id,
            vendor,
            product,
            endpoint,
            features: Features {
                level: dimmable.contains(&endpoint),
                hue_saturation: capability & CAPABILITY_HUE_SATURATION != 0,
                xy: capability & CAPABILITY_XY != 0,
                mireds,
            },
            address: None,
        })
    }

    async fn read(&self, target: &Target, attributes: &[(u32, u32)]) -> Result<LightState, Error> {
        self.prefer(target);
        let paths: Vec<AttrPath> = attributes
            .iter()
            .map(|(cluster, attribute)| path(Some(target.endpoint), *cluster, *attribute))
            .collect();
        let mut state = LightState::default();
        let endpoint = target.endpoint;
        self.read_paths(target.node_id, &paths, |resp| {
            let byte = |cluster, attribute| first::<u8>(resp, endpoint, cluster, attribute);
            let word = |cluster, attribute| first::<u16>(resp, endpoint, cluster, attribute);
            if let Some(on) = first::<bool>(resp, endpoint, light::ON_OFF, light::ON_OFF_ON) {
                state.on = Some(on);
            }
            if let Some(level) =
                first::<Nullable<u8>>(resp, endpoint, light::LEVEL_CONTROL, light::LEVEL_CURRENT)
            {
                state.level = level.into_option().or(state.level);
            }
            state.hue = byte(light::COLOR_CONTROL, light::COLOR_HUE).or(state.hue);
            state.saturation =
                byte(light::COLOR_CONTROL, light::COLOR_SATURATION).or(state.saturation);
            state.x = word(light::COLOR_CONTROL, light::COLOR_X).or(state.x);
            state.y = word(light::COLOR_CONTROL, light::COLOR_Y).or(state.y);
            state.mireds = word(light::COLOR_CONTROL, light::COLOR_MIREDS).or(state.mireds);
            state.color_mode = byte(light::COLOR_CONTROL, light::COLOR_MODE).or(state.color_mode);
        })
        .await?;
        Ok(state)
    }

    /// One read transaction; `collect` sees every chunk of the report.
    async fn read_paths(
        &self,
        node_id: u64,
        paths: &[AttrPath],
        mut collect: impl FnMut(&ReportDataResp<'_>),
    ) -> Result<(), Error> {
        let exchange = Exchange::initiate(self.matter, self.crypto, self.fab_idx, node_id).await?;
        let mut chunk = exchange
            .read_with(|request| {
                request
                    .attr_requests_from(paths)?
                    .fabric_filtered(true)?
                    .end()
            })
            .await?;
        loop {
            collect(&chunk.response()?);
            match chunk.complete().await? {
                Some(next) => chunk = next,
                None => return Ok(()),
            }
        }
    }

    async fn invoke(&self, target: &Target, action: Action) -> Result<(), Error> {
        self.prefer(target);
        let exchange =
            Exchange::initiate(self.matter, self.crypto, self.fab_idx, target.node_id).await?;
        let endpoint = target.endpoint;
        // Colour changes apply to a lamp that is off too, ready for when it turns on.
        let color_options = color_control::OptionsBitmap::EXECUTE_IF_OFF;
        match action {
            Action::On => exchange.on_off().on(endpoint).await,
            Action::Off => exchange.on_off().off(endpoint).await,
            Action::Level(level) => {
                exchange
                    .level_control()
                    .move_to_level_with_on_off(endpoint, |request| {
                        request
                            .level(level)?
                            .transition_time(Nullable::some(0))?
                            .options_mask(level_control::OptionsBitmap::empty())?
                            .options_override(level_control::OptionsBitmap::empty())?
                            .end()
                    })
                    .await
            }
            Action::HueSaturation { hue, saturation } => {
                exchange
                    .color_control()
                    .move_to_hue_and_saturation(endpoint, |request| {
                        request
                            .hue(hue)?
                            .saturation(saturation)?
                            .transition_time(0)?
                            .options_mask(color_options)?
                            .options_override(color_options)?
                            .end()
                    })
                    .await
            }
            Action::Xy { x, y } => {
                exchange
                    .color_control()
                    .move_to_color(endpoint, |request| {
                        request
                            .color_x(x)?
                            .color_y(y)?
                            .transition_time(0)?
                            .options_mask(color_options)?
                            .options_override(color_options)?
                            .end()
                    })
                    .await
            }
            Action::Mireds(mireds) => {
                exchange
                    .color_control()
                    .move_to_color_temperature(endpoint, |request| {
                        request
                            .color_temperature_mireds(mireds)?
                            .transition_time(0)?
                            .options_mask(color_options)?
                            .options_override(color_options)?
                            .end()
                    })
                    .await
            }
        }
    }

    /// Takes Hestia's fabric off the device, freeing its slot.
    async fn unpair(&self, target: &Target) -> Result<(), String> {
        self.prefer(target);
        let removal = async {
            let exchange =
                Exchange::initiate(self.matter, self.crypto, self.fab_idx, target.node_id).await?;
            let index = exchange
                .operational_credentials()
                .current_fabric_index_read(ROOT_ENDPOINT)
                .await?;
            let exchange =
                Exchange::initiate(self.matter, self.crypto, self.fab_idx, target.node_id).await?;
            let handle = exchange
                .operational_credentials()
                .remove_fabric(ROOT_ENDPOINT, |request| request.fabric_index(index)?.end())
                .await?;
            let status = handle.response()?.status_code()?;
            // The device drops the session as soon as it answers: no final acknowledgement.
            let _ = handle.complete().await;
            if status == NodeOperationalCertStatusEnum::OK {
                Ok(())
            } else {
                Err(Error::from(ErrorCode::Failure))
            }
        };
        removal.await.map_err(|error: Error| error.to_string())
    }

    /// Tries the address the driver remembers before asking mDNS.
    fn prefer(&self, target: &Target) {
        if let Some(address) = target.address {
            self.book.borrow_mut().insert(target.node_id, address);
        }
    }

    /// On success, the address the device was reached at. rs-matter expires a session whose
    /// peer stopped answering by itself, so a failure only needs the address forgotten.
    fn settle<T>(&self, target: &Target, result: Result<T, String>) -> Result<Answer<T>, String> {
        match result {
            Ok(value) => Ok(Answer {
                value,
                address: self.book.borrow().get(&target.node_id).copied(),
            }),
            Err(error) => {
                debug!(node = format!("{:016x}", target.node_id), %error, "Matter exchange failed");
                self.book.borrow_mut().remove(&target.node_id);
                Err(error)
            }
        }
    }
}

fn path(endpoint: Option<u16>, cluster: u32, attribute: u32) -> AttrPath {
    AttrPath {
        endpoint,
        cluster: Some(cluster),
        attr: Some(attribute),
        ..AttrPath::default()
    }
}

fn first<'a, T: FromTLV<'a> + 'a>(
    resp: &ReportDataResp<'a>,
    endpoint: u16,
    cluster: u32,
    attribute: u32,
) -> Option<T> {
    resp.attrs::<T>(cluster, attribute)
        .find(|(found, _)| *found == endpoint)
        .and_then(|(_, value)| value.ok())
}

fn endpoints<T>(reports: impl Iterator<Item = (u16, Result<T, Error>)>) -> Vec<u16> {
    reports
        .filter(|(_, value)| value.is_ok())
        .map(|(endpoint, _)| endpoint)
        .collect()
}

fn values<T>(reports: impl Iterator<Item = (u16, Result<T, Error>)>) -> Vec<(u16, T)> {
    reports
        .filter_map(|(endpoint, value)| value.ok().map(|value| (endpoint, value)))
        .collect()
}

async fn within<T>(
    limit: Duration,
    work: impl Future<Output = Result<T, impl std::fmt::Display>>,
) -> Result<T, String> {
    match select(work, async_io::Timer::after(limit)).await {
        Either::First(result) => result.map_err(|error| error.to_string()),
        Either::Second(_) => Err(format!("no answer within {} s", limit.as_secs())),
    }
}

#[cfg(test)]
mod tests {
    use super::SetupCode;

    #[test]
    fn setup_codes() {
        // The test device's code (passcode 20202021, discriminator 3840).
        let code = SetupCode::parse("3497-011-2332").unwrap();
        assert_eq!(code.passcode, 20_202_021);
        assert_eq!(code.filter.short_discriminator, Some(15));
        assert!(SetupCode::parse("MT:not-base38").is_err());
        assert!(SetupCode::parse("1234").is_err());
        assert!(SetupCode::parse("34970112331").is_err(), "bad check digit");
    }
}
