//! Owner capabilities and lifecycle policy for trusted process supervisors.
//! Includes owner/bootstrap pairs, recovery tickets, takeover preparation and
//! owner status; Core retains lifecycle and capability authority.

pub use c2_core::{
    OwnerBootstrap, OwnerControl, OwnerControlError, OwnerControlErrorKind, OwnerPhase,
    OwnerRecoveryTicket, OwnerStatus, PreparedOwnerTakeover, ServerLifecyclePolicy,
    owner_control_pair,
};
