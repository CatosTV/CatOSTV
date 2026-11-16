use argon2::{
    Argon2, Params, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString,
};
use catostv_core::hidden_apps::Pin;
use std::{
    io::Read,
    time::{Duration, Instant},
};

pub fn create(pin: &Pin) -> Result<String, String> {
    if !pin.valid() {
        return Err("Use four digits".into());
    }
    let mut random = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut random))
        .map_err(|_| "Could not create PIN".to_owned())?;
    let salt = SaltString::encode_b64(&random).map_err(|_| "Could not create PIN".to_owned())?;
    Argon2::default()
        .hash_password(pin.as_str().as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| "Could not create PIN".into())
}
pub fn verify(pin: &Pin, encoded: &str) -> Result<bool, String> {
    if !pin.valid() {
        return Ok(false);
    }
    let hash = PasswordHash::new(encoded).map_err(|_| "Could not read PIN settings".to_owned())?;
    // Only the parameters this app writes are accepted, so a damaged config
    // cannot request unbounded hashing work.
    if hash.algorithm.as_str() != "argon2id"
        || hash.version != Some(19)
        || hash.params.get_decimal("m") != Some(Params::DEFAULT_M_COST)
        || hash.params.get_decimal("t") != Some(Params::DEFAULT_T_COST)
        || hash.params.get_decimal("p") != Some(Params::DEFAULT_P_COST)
    {
        return Err("Could not read PIN settings".into());
    }
    Ok(Argon2::default()
        .verify_password(pin.as_str().as_bytes(), &hash)
        .is_ok())
}
#[derive(Default)]
pub struct Attempts {
    failures: u8,
    blocked_until: Option<Instant>,
}
impl Attempts {
    pub fn wait(&self, now: Instant) -> u64 {
        self.blocked_until
            .filter(|until| *until > now)
            .map(|until| until.duration_since(now).as_secs().saturating_add(1))
            .unwrap_or(0)
    }
    pub fn record(&mut self, passed: bool, now: Instant) -> u64 {
        if passed {
            self.failures = 0;
            self.blocked_until = None;
            return 0;
        }
        self.failures = self.failures.saturating_add(1);
        if self.failures >= 5 {
            self.failures = 0;
            self.blocked_until = Some(now + Duration::from_secs(30));
        }
        self.wait(now)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn pin(value: &str) -> Pin {
        let mut entry =
            catostv_core::hidden_apps::Entry::new(1, catostv_core::hidden_apps::Goal::Browse, true);
        value
            .chars()
            .find_map(
                |c| match entry.handle(catostv_core::input::Action::Char(c)) {
                    Some(catostv_core::hidden_apps::Request::Verify(pin)) => Some(pin),
                    _ => None,
                },
            )
            .unwrap()
    }
    #[test]
    fn pins_use_random_salts_and_only_the_matching_pin_opens_them() {
        let good = pin("7391");
        let wrong = pin("7392");
        let a = create(&good).unwrap();
        let b = create(&good).unwrap();
        assert_ne!(a, b);
        assert!(!a.contains("7391"));
        assert!(verify(&good, &a).unwrap());
        assert!(!verify(&wrong, &a).unwrap());
        assert!(verify(&good, "invalid").is_err());
        assert!(verify(&good, &a.replace("m=19456", "m=99999999")).is_err());
    }
    #[test]
    fn attempts_are_limited_across_visits_and_expire() {
        let now = Instant::now();
        let mut attempts = Attempts::default();
        for _ in 0..4 {
            assert_eq!(attempts.record(false, now), 0);
        }
        assert!(attempts.record(false, now) >= 30);
        assert_eq!(attempts.wait(now + Duration::from_secs(31)), 0);
        attempts.record(true, now);
        assert_eq!(attempts.wait(now), 0);
    }
}
