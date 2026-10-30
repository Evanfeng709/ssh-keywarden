//! 规则层：只做「文本 → 结构 → 文本」的转换，不碰网络与 UI。
//!
//! 桌面端与 Android 端共用这一层；HTTP/SSH 细节在 `kw-server` / `kw-ssh`。

pub mod authorized;
pub mod backups;
pub mod keygen;
pub mod localkeys;
pub mod pathutil;
pub mod sshd;

pub use authorized::{
    append_key, parse_authorized_keys, render_without, AppendError, AppendOutcome, AuthorizedItem,
    AuthorizedKeys, ItemData, ItemKind,
};
pub use localkeys::{find_private_key_by_fingerprint, scan_keys, LocalKeyEntry, LocalKeyKind};
pub use keygen::{
    fingerprint_of_blob, generate_ed25519, generate_rsa, inspect_private_key, load_private_key,
    normalize_comment, parse_public_key_line, parse_public_keys, private_key_openssh, GeneratedKey,
    KeyGenError, ParsedPublicKey, PrivateKeyInfo,
};
