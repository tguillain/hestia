use argon2::password_hash::SaltString;
use argon2::password_hash::rand_core::OsRng;
use argon2::{Algorithm, Argon2, Params, PasswordHash, PasswordHasher, PasswordVerifier, Version};

/// Argon2id with the crate's recommended parameters.
///
/// # Errors
/// Fails only if the system random generator is unavailable.
pub fn hash_password(password: &str) -> anyhow::Result<String> {
    hash(password, &Argon2::default())
}

/// Same as [`hash_password`] with explicit cost, for tests.
///
/// # Errors
/// Fails on invalid parameters.
pub fn hash_password_with(
    password: &str,
    memory_kib: u32,
    iterations: u32,
) -> anyhow::Result<String> {
    let params =
        Params::new(memory_kib, iterations, 1, None).map_err(|error| anyhow::anyhow!("{error}"))?;
    hash(
        password,
        &Argon2::new(Algorithm::Argon2id, Version::V0x13, params),
    )
}

fn hash(password: &str, argon2: &Argon2<'_>) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    argon2
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|error| anyhow::anyhow!("cannot hash password: {error}"))
}

/// Parameters are read from the stored hash, so old hashes keep verifying.
pub(crate) fn verify_password(password: &str, stored: &str) -> bool {
    PasswordHash::new(stored).is_ok_and(|hash| {
        Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .is_ok()
    })
}

#[cfg(test)]
mod tests {
    use super::{hash_password_with, verify_password};

    #[test]
    fn verifies_only_the_right_password() {
        let hash = hash_password_with("correct horse", 64, 1).unwrap();
        assert!(verify_password("correct horse", &hash));
        assert!(!verify_password("wrong", &hash));
        assert!(!verify_password("correct horse", "not a hash"));
    }
}
