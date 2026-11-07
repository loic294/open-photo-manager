//! Filter validation and normalisation.
//!
//! The wire types [`Filter`](crate::dto::Filter) and [`Scope`](crate::dto::Scope)
//! live in `dto/common.rs`; this module will hold the pure logic around them
//! (trimming and de-duplicating values, clamping ratings to 0-5, canonical
//! ordering, rejecting absolute or `..` directories).
//!
//! Stub, owned by todo `implementing-core-domain` (agent B).
