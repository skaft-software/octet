"""Pure parser/domain tests. Handcrafted input is NOT a recorded ngspice run.

No solver mock, fake executable, fallback generator, or F01/F02 acceptance here.
"""
import math
import unittest

from solver import (Circuit, SimulationSession, MAX_OUTPUT_BYTES, MAX_WAVEFORM_BYTES,
                    SAMPLE, SpiceError, measure, parse_print)

# Deliberately tiny handwritten parser fixture; not simulator evidence.
PRINT_FIXTURE = b"""Circuit: parser fixture only
Transient Analysis
Index   time             v(out)
--------------------------------------------------------------------------------
0       1.000000e-07      9.9990001e-05
1       1.000000e-03      6.321205588286e-01
\fpage heading
Index   time             v(out)
--------------------------------------------------------------------------------
2       5.000000e-03      9.932620530009e-01
Total analysis time (seconds) = 0.001
"""


class ParserDomainTests(unittest.TestCase):
    def test_paginated_print_and_finite_scalar_measurement(self):
        waveform = parse_print(PRINT_FIXTURE)
        result = measure(waveform)
        self.assertEqual(len(waveform), 3 * SAMPLE.size)
        self.assertEqual(result.samples, 3)
        self.assertAlmostEqual(result.final_time_s, 0.005)
        self.assertAlmostEqual(result.expected_voltage_v, 1 - math.exp(-5))
        self.assertLess(result.absolute_error_v, 1e-10)

    def test_malformed_solver_output_is_not_a_waveform(self):
        invalid = [b"", b"\xff", b"ngspice failed", PRINT_FIXTURE + b"3 0.006 1\n",
                   PRINT_FIXTURE.replace(b"v(out)", b"v(in)"),
                   PRINT_FIXTURE.replace(b"1       1.000000e-03", b"9       1.000000e-03"),
                   PRINT_FIXTURE.replace(b"9.932620530009e-01", b"nan"),
                   PRINT_FIXTURE.replace(b"5.000000e-03", b"inf"),
                   PRINT_FIXTURE.replace(b"9.932620530009e-01", b"2.0"),
                   PRINT_FIXTURE.replace(b"5.000000e-03", b"2.000000e-03"),
                   PRINT_FIXTURE.replace(b"6.321205588286e-01", b"bad"),
                   PRINT_FIXTURE.replace(b"1.000000e-03", b"1.000000e-07")]
        for output in invalid:
            with self.subTest(output=output[:40]), self.assertRaises(SpiceError):
                parse_print(output)

    def test_source_output_and_binary_bounds(self):
        with self.assertRaises(SpiceError):
            parse_print(b"x" * (MAX_OUTPUT_BYTES + 1))
        for waveform in (b"", b"x", b"x" * (MAX_WAVEFORM_BYTES + 1), SAMPLE.pack(0, 0)):
            with self.subTest(length=len(waveform)), self.assertRaises(SpiceError):
                measure(waveform)

    def test_binary_validation_rejects_nonphysical_or_partial_runs(self):
        for rows in (((0, 0), (0.005, 0.5)), ((0, 0), (0.001, 0.63)),
                     ((0.002, 0.8), (0.001, 0.9), (0.005, 0.993262)),
                     ((0, 0), (0.004, 0.99999), (0.005, 0.993262)),
                     ((0, 0), (0.005, float("nan"))), ((-0.001, 0), (0.005, 0.993262))):
            with self.subTest(rows=rows), self.assertRaises(SpiceError):
                measure(b"".join(SAMPLE.pack(*row) for row in rows))

    def test_native_session_does_not_alias_mutable_circuit(self):
        circuit = Circuit()
        session = SimulationSession(circuit)
        self.assertIn(".print tran v(out)", session.netlist)
        circuit.close()
        self.assertNotEqual(circuit.netlist, session.netlist)
        self.assertEqual(session.completed_runs, 0)
        session.close()
        self.assertEqual(session.netlist, "")


if __name__ == "__main__":
    unittest.main()
