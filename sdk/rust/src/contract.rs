//! Contract tooling for release validation and route identity. Use these types
//! when inspecting descriptors, selecting validation limits or binding a release
//! to a runtime route; ordinary applications can start with `ContractRelease`.

pub use c2_contract::{
    ContractError, ContractLimits, ContractLimitsProfile, ContractReleaseRef, ExpectedRouteContract,
};
