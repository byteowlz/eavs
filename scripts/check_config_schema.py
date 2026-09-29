#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = ["jsonschema==4.26.0"]
# ///
"""Validate local fixtures with standard Draft 7 JSON Schema; no provider calls.

Run: uv run scripts/check_config_schema.py
Dependency installation may need network; validation itself is entirely offline.
Field coverage is a limited source scanner, not generated serde/schema parity.
"""
import jsonschema
import json
from pathlib import Path
import re
import tomllib

ROOT = Path(__file__).resolve().parents[1]
SCHEMA = ROOT / "config/config.schema.json"
def validate(value, schema):
    jsonschema.Draft7Validator.check_schema(schema)
    jsonschema.Draft7Validator(
        schema, format_checker=jsonschema.FormatChecker()
    ).validate(value)


def field_coverage(schema):
    """Detect added/removed/renamed fields in mapped structs (not enum variants).

    Flattened MockResponsesConfig is checked separately. Types, defaults, serde
    aliases and conditional compilation are NOT inferred by this source scanner.
    """
    p, d = schema["properties"], schema["$defs"]
    model = d["provider"]["properties"]["models"]["items"]
    mapping = {"AppConfig": schema, "ProviderConfig": d["provider"],
               "ModelShortlistEntry": model, "ModelCost": model["properties"]["cost"],
               "AnalysisPluginConfig": d["analysisPlugin"],
               "TransformPluginConfig": d["transformPlugin"],
               "MockResponse": d["mockResponse"],
               "TrustedPrivateEndpoint": p["network"]["properties"]["trusted_private_endpoints"]["items"]}
    for name, key in [("Server", "server"), ("Logging", "logging"), ("Analysis", "analysis"),
                      ("Transform", "transform"), ("State", "state"), ("Keys", "keys"),
                      ("Capture", "capture"), ("Network", "network"), ("Egress", "egress"),
                      ("NativePricing", "native_pricing")]:
        mapping[name + "Config"] = p[key]
    source = (ROOT / "src/config.rs").read_text()
    for name, node in mapping.items():
        body = re.search(r"pub struct " + name + r"\s*\{(.*?)\n\}", source, re.S)
        if body is None:
            raise ValueError(f"Cannot locate struct {name}")
        fields = set(re.findall(r"^\s*pub (\w+):", body[1], re.M))
        # Explicit rename handling: intentionally narrow, not a Rust parser.
        if name == "ProviderConfig":
            fields.remove("type_")
            fields.add("type")
        if name == "MockResponse":
            fields.remove("response_type")
            fields.add("type")
        documented = set(node.get("properties", {}))
        if fields != documented:
            raise ValueError(f"{name} field drift: missing={fields - documented}, extra={documented - fields}")
    body = re.search(r"pub struct MockResponsesConfig\s*\{(.*?)\n\}", source, re.S)[1]
    if not re.search(r"#\[serde\(flatten\)\]\s*pub responses: HashMap<String, MockResponse>", body):
        raise ValueError("MockResponsesConfig flatten mapping changed")


def main():
    schema = json.loads(SCHEMA.read_text())
    field_coverage(schema)
    for example in schema["examples"]:
        validate(example, schema)
    with (ROOT / "config/config.example.toml").open("rb") as example:
        validate(tomllib.load(example), schema)
    print("PASS: offline config.example.toml validation and mapped Rust struct field coverage")


if __name__ == "__main__":
    main()
