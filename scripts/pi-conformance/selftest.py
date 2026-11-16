#!/usr/bin/env python3
"""Offline scoring regressions; no CLIs or extension factories are executed."""
import json
from pathlib import Path
import unittest
from unittest.mock import patch
from conformance import SCENARIOS, compare, s_generic, start, verdict


class Scoring(unittest.TestCase):
    def columns(self, value=True):
        return {host: {'obs': {'turn_finished': value}, 'agent_dir': {'added': [], 'changed': [], 'deleted': []}}
                for host in ('pi', 'current')}

    def test_equal_false_is_not_success(self):
        columns = self.columns(False)
        self.assertTrue(compare(columns)[0]['current_matches_pi'])
        self.assertEqual(verdict(columns)['status'], 'fail')

    def test_both_hosts_must_execute(self):
        columns = self.columns()
        del columns['pi']
        self.assertEqual(verdict(columns)['status'], 'fail')

    def test_reference_failure_is_not_an_offline_skip(self):
        columns = self.columns()
        columns['pi'] = {'error': 'reference crashed'}
        self.assertEqual(verdict(columns)['status'], 'fail')

    def test_unexpected_error_cannot_be_a_known_deviation(self):
        columns = self.columns()
        columns['current'] = {'error': 'unrelated crash'}
        self.assertEqual(verdict(columns)['status'], 'fail')

    def test_protected_writes_fail(self):
        columns = self.columns()
        columns['current']['agent_dir']['changed'] = ['auth.json']
        self.assertEqual(verdict(columns)['status'], 'fail')

    def test_success(self):
        self.assertEqual(verdict(self.columns())['status'], 'pass')

    def test_start_waits_for_native_extension_readiness_not_only_editor_rules(self):
        class Terminal:
            screens = 0
            def pump(self, _seconds):
                pass
            def screen(self):
                self.screens += 1
                return '\n'.join(['─' * 40, 'Loading your Pi setup…' if self.screens == 1 else '', '─' * 40])
        terminal = Terminal()
        with patch('conformance.drive.Term', return_value=terminal):
            start(object())
        self.assertEqual(terminal.screens, 2)

    def test_generic_journey_requires_a_completed_turn(self):
        with patch('conformance.settle', return_value=''), patch('conformance.ask', return_value=False):
            observations, _ = s_generic(object(), object())
        self.assertIs(observations.get('turn_finished'), False)

    def test_representative_matrix_names_executable_source_cited_journeys(self):
        matrix = json.loads(Path(__file__).with_name('journeys.json').read_text())
        self.assertTrue(set(matrix) <= set(SCENARIOS))
        surfaces = {surface for row in matrix.values() for surface in row['surfaces']}
        self.assertTrue({'commands', 'tools', 'input hooks', 'context hooks', 'provider hooks',
                         'custom editors', 'UI components', 'UI panels', 'session operations',
                         'compaction', 'model switching'} <= surfaces)
        for row in matrix.values():
            self.assertTrue(row['pi_source'])
            self.assertTrue(all(':' in citation for citation in row['pi_source']))


if __name__ == '__main__':
    unittest.main()
