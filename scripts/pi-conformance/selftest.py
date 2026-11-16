#!/usr/bin/env python3
"""Offline scoring regressions; no CLIs or extension factories are executed."""
import unittest
from conformance import compare, verdict


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


if __name__ == '__main__':
    unittest.main()
