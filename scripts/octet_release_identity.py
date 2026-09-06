"""Repository identity for current releases and the immutable pre-rename release."""

CANONICAL_REPOSITORY = "skaft-software/octet"
LEGACY_RELEASE_COMMIT = "6dcde0620314c554b11719b0bc97835b104f0e47"


def release_repository(version: str, source_commit: str, workflow_commit: str) -> str:
    """Keep v0.7.0's original signing identity; never broaden future trust."""
    if (
        version == "0.7.0"
        and source_commit == LEGACY_RELEASE_COMMIT
        and workflow_commit == LEGACY_RELEASE_COMMIT
    ):
        return "skaft-software/ygg"
    return CANONICAL_REPOSITORY
