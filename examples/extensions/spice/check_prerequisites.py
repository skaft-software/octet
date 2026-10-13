"""Prerequisites only, NOT an F01/F02 conformance runner."""
import sys

from solver import SpiceError, require_ngspice


def main() -> int:
    try:
        require_ngspice()
    except SpiceError as error:
        print(error, file=sys.stderr)
        return 2
    print("READY: ngspice found on PATH. F01/F02 have NOT been executed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
