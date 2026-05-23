//! Validated username target for site checks.

use std::fmt;
use std::str::FromStr;

use crate::error::{Error, Result};

/// Maximum username length we accept.
///
/// Sites in the wild generally cap usernames around 30 characters; 64 leaves
/// headroom for less common services while keeping URLs sane.
const MAX_LEN: usize = 64;

/// A validated username.
///
/// The character set is restricted to ASCII letters, digits, `_`, `-`, and
/// `.`. This keeps URL substitution naive (no percent-encoding needed) and
/// guards against accidental cross-site normalisation differences. Usernames
/// containing characters outside this set are rejected at construction.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Username(String);

impl Username {
    /// Construct a `Username`, validating its character set and length.
    pub fn new(input: impl Into<String>) -> Result<Self> {
        let input = input.into();
        if let Some(reason) = invalid_reason(&input) {
            return Err(Error::InvalidUsername { input, reason });
        }
        Ok(Self(input))
    }

    /// Borrow the inner string.
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Username {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Username {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::new(s)
    }
}

impl AsRef<str> for Username {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl serde::Serialize for Username {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for Username {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

fn invalid_reason(s: &str) -> Option<String> {
    if s.is_empty() {
        return Some(String::from("username is empty"));
    }
    if s.len() > MAX_LEN {
        return Some(format!("username exceeds {MAX_LEN} characters"));
    }
    s.chars()
        .find(|c| !is_allowed(*c))
        .map(|c| format!("contains invalid character {c:?}"))
}

#[inline]
const fn is_allowed(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_common_usernames() {
        for ok in ["alice", "bob_doe", "user-name", "a.b", "1234", "A_b-c.d"] {
            assert!(Username::new(ok).is_ok(), "{ok:?} should be accepted");
        }
    }

    #[test]
    fn rejects_empty() {
        let err = Username::new("").unwrap_err();
        assert!(matches!(err, Error::InvalidUsername { .. }));
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn rejects_too_long() {
        let long = "a".repeat(MAX_LEN + 1);
        assert!(Username::new(long).is_err());
        let edge = "a".repeat(MAX_LEN);
        assert!(
            Username::new(edge).is_ok(),
            "exactly {MAX_LEN} chars is allowed"
        );
    }

    #[test]
    fn rejects_disallowed_characters() {
        for bad in [
            " alice", "alice ", "ali ce", "a/b", "a?b", "a#b", "ali@ce", "café",
        ] {
            assert!(Username::new(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn display_and_as_str_roundtrip() {
        let u = Username::new("alice").unwrap();
        assert_eq!(u.as_str(), "alice");
        assert_eq!(u.to_string(), "alice");
        assert_eq!(<Username as AsRef<str>>::as_ref(&u), "alice");
    }

    #[test]
    fn from_str_works() {
        let u: Username = "carol".parse().unwrap();
        assert_eq!(u.as_str(), "carol");
    }

    #[test]
    fn serde_roundtrip_via_json() {
        let u = Username::new("dave_42").unwrap();
        let json = serde_json::to_string(&u).unwrap();
        assert_eq!(json, "\"dave_42\"");
        let back: Username = serde_json::from_str(&json).unwrap();
        assert_eq!(back, u);
    }

    #[test]
    fn serde_deserialize_validates() {
        let err = serde_json::from_str::<Username>("\"bad space\"").unwrap_err();
        assert!(err.to_string().contains("invalid character"));
    }

    proptest::proptest! {
        /// Validation must never panic on arbitrary input — only Ok/Err.
        #[test]
        fn new_never_panics_on_arbitrary_input(s in ".*") {
            let _ = Username::new(s);
        }

        /// Any string matching the allowed charset/length is accepted and
        /// round-trips losslessly through `as_str` and serde.
        #[test]
        fn valid_usernames_round_trip(s in "[A-Za-z0-9._-]{1,64}") {
            let u = Username::new(s.clone()).expect("matches the username charset");
            proptest::prop_assert_eq!(u.as_str(), s.as_str());
            let json = serde_json::to_string(&u).unwrap();
            let back: Username = serde_json::from_str(&json).unwrap();
            proptest::prop_assert_eq!(back, u);
        }

        /// Any string containing a disallowed character is rejected.
        #[test]
        fn strings_with_disallowed_chars_are_rejected(s in "[A-Za-z0-9._-]{0,20}[^A-Za-z0-9._-][A-Za-z0-9._-]{0,20}") {
            proptest::prop_assert!(Username::new(s).is_err());
        }
    }
}
