#!/bin/sh
# Keep protocol, source validation and conversion in the version-matched host.
# exec preserves protocol stdio and the host's process supervision boundary.
exec octet migrate adapter pi
