//! Exit codes beyond success and failure; the Mac app maps them to outcomes
//! (ADR "Menu bar app: Swift shell over the ddc CLI", §6).

/// The request was refused before anything was touched.
pub const REFUSED: u8 = 2;
/// Another scan or clean holds the data-directory lock (`EX_TEMPFAIL`).
pub const BUSY: u8 = 75;
/// The platform is not supported, or lacks the capability the command needs
/// (`EX_UNAVAILABLE`; ADR "Platform seam: one platform module per OS with
/// declared capabilities", §6).
pub const UNSUPPORTED: u8 = 69;
