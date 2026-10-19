#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = ["jsonschema==4.26.0"]
# ///
"""Offline schema regressions: uv run scripts/test_config_schema.py.

These tests cover selected types/constraints and mapped struct field presence,
not full serde parity, enum/default drift, or application runtime validation.
"""
import copy
import json
import unittest

import jsonschema
from check_config_schema import SCHEMA, field_coverage, main, validate


class ConfigSchemaTests(unittest.TestCase):
    def setUp(self):
        self.schema = json.loads(SCHEMA.read_text())

    def test_examples_and_field_coverage(self):
        main()

    def test_misplaced_network_regression(self):
        self.schema['network'] = self.schema['properties'].pop('network')
        with self.assertRaises((KeyError, ValueError)):
            field_coverage(self.schema)

    def test_nested_field_removal_regressions(self):
        paths = [
            ('properties', 'egress', 'properties', 'dns_port'),
            ('properties', 'inference', 'properties', 'cors_allowed_origins'),
            ('properties', 'native_pricing', 'properties', 'jobs'),
            ('$defs', 'mockResponse', 'properties', 'arguments'),
            ('$defs', 'transformPlugin', 'properties', 'timeout_ms'),
        ]
        for path in paths:
            with self.subTest(path=path):
                schema = copy.deepcopy(self.schema)
                node = schema
                for key in path[:-1]:
                    node = node[key]
                del node[path[-1]]
                with self.assertRaises(ValueError):
                    field_coverage(schema)

    def test_nested_invalid_values(self):
        invalid = [
            {'network': {'allow_domains': [1]}},
            {'inference': {'listen': 3035}},
            {'inference': {'cors_allowed_origins': [1]}},
            {'inference': {'typo': True}},
            {'network': {'trusted_private_endpoints': [{'host': 'x', 'port': 80}]}},
            {'network': {'trusted_private_endpoints': [{'host': 'x', 'port': 80, 'address': 'example.org'}]}},
            {'egress': {'port': 65536}},
            {'egress': {'port': -1}},
            {'egress': {'port': True}},
            {'egress': {'enforce': 'yes'}},
            {'native_pricing': {'jobs': {'model': 'free'}}},
            {'mock_responses': {'example': {'arguments': {}}}},
            {'mock_responses': {'example': {'status': 65536}}},
            {'transform': {'plugins': [{'env': {'X': 1}}]}},
            {'transform': {'plugins': [{'providers': [False]}]}},
            {'transform': {'plugins': [{'timeout_ms': -1}]}},
        ]
        for value in invalid:
            with self.subTest(value=value):
                with self.assertRaises(jsonschema.ValidationError):
                    validate(value, self.schema)


if __name__ == '__main__':
    unittest.main()
