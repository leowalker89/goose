//! Auth mode classification stub.
//!
//! The full auth_mode system classifies request authentication type.
//! For headroom integration, we provide a simple enum.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthMode {
    /// Pay-as-you-go API key
    Payg,
    /// OAuth bearer token
    OAuth,
    /// Subscription seat
    Subscription,
}
