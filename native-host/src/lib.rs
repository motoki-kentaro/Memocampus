//! What the host and VisibleStore share: the privacy policy, the vault schema definition and the
//! rules for page metadata and navigation facts, and the active vault's config and publication.
pub mod active_vault;
pub mod domain_approvals;
pub mod navigation;
pub mod page_metadata;
pub mod policy;
pub mod policy_store;
pub mod public_suffix;
pub mod schema;
pub mod secure_vault;
