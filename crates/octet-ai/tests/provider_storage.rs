//! Independent provider storage has no startup effects.
use octet_ai::provider_auth::codex::{CodexResolver, CredentialStore};
use octet_ai::CredentialResolver;

#[tokio::test]
async fn independent_storage_is_inert_and_missing_login_is_safe() {
    let memory = CredentialStore::memory();
    assert!(!memory.is_signed_in().unwrap());
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("private/codex.json");
    let file = CredentialStore::new(&path);
    let resolver = CodexResolver::new(file.clone());
    assert!(!path.parent().unwrap().exists());
    assert!(!file.is_signed_in().unwrap());
    assert!(resolver.resolve().await.is_err());
    assert!(!path.parent().unwrap().exists());
    assert!(!format!("{resolver:?}").contains("Bearer"));
}
