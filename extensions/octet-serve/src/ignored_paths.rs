//! Directories that project indexing and search never descend into.
//!
//! One rule for the `@` context picker's index and the Files tab search, so
//! the two backends agree: build output, dependencies, VCS state, hidden
//! directories (except `.github`) and secret stores.

/// Whether a directory named `name` is skipped.
pub(crate) fn ignored_directory(name: &str) -> bool {
    let lowercase = name.to_ascii_lowercase();
    lowercase.starts_with('.') && lowercase != ".github"
        || matches!(
            lowercase.as_str(),
            "node_modules"
                | "target"
                | "dist"
                | "build"
                | "out"
                | "coverage"
                | "vendor"
                | "__pycache__"
                | ".git"
                | ".hg"
                | ".svn"
                | ".idea"
                | ".vscode"
                | ".ssh"
                | ".aws"
                | ".azure"
                | ".gnupg"
                | ".kube"
                | "secrets"
                | "credentials"
                | "private-keys"
        )
}
