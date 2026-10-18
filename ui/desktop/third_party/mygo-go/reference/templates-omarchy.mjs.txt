#!/usr/bin/env node
/**
 * Omarchy theme adapter — maps the machine's active omarchy theme onto the
 * byteowlz design-system slot vars (src/theme/omarchy.generated.css).
 *
 * Mapping contract (byteowlz govnr ADR-0040, "Omarchy compatibility fixture"):
 * the omarchy canonical semantic palette maps deterministically onto base24 —
 *   background family + selection + muted  → base00..base07 (with foregrounds)
 *   red/orange/yellow/green/cyan/blue/magenta/brown → base08..base0F
 *   dark/darker backgrounds + bright accents        → base10..base17
 * `mode` and `accent` stay explicit semantic metadata (--omarchy-mode/...-accent).
 *
 * Usage:
 *   node scripts/omarchy-theme.mjs            # regenerate (exit 0 even without omarchy)
 *   node scripts/omarchy-theme.mjs --watch    # regenerate on theme change
 *   OMARCHY_THEME_FILE=/path/colors.toml ...  # explicit theme file
 *
 * No dependencies. Works under node or bun.
 */

import { readFileSync, realpathSync, existsSync, writeFileSync, mkdirSync } from "node:fs";
import { homedir } from "node:os";
import path from "node:path";

const OUT_FILE = path.resolve(import.meta.dirname ?? ".", "../src/theme/omarchy.generated.css");

function themeCandidates() {
	const list = [];
	if (process.env.OMARCHY_THEME_FILE) list.push(process.env.OMARCHY_THEME_FILE);
	list.push(
		path.join(homedir(), ".local/state/omarchy/current/theme/colors.toml"),
		path.join(homedir(), ".config/omarchy/current/theme/colors.toml"), // legacy symlink
	);
	return list;
}

function findThemeFile() {
	for (const p of themeCandidates()) {
		try {
			if (p && existsSync(p)) return p;
		} catch {}
	}
	return null;
}

/** Minimal parser for omarchy's flat `key = "value"` colors.toml. */
function parseColorsToml(text) {
	const out = {};
	for (const line of text.split("\n")) {
		const m = line.match(/^\s*([A-Za-z0-9_]+)\s*=\s*"([^"]*)"\s*(?:#.*)?$/);
		if (m) out[m[1]] = m[2];
	}
	return out;
}

/** Canonical omarchy keys → base24 slots (ADR-0040 deterministic map). */
const CANONICAL = {
	base00: "background",
	base01: "lighter_background",
	base02: "selection",
	base03: "muted",
	base04: "dark_foreground",
	base05: "foreground",
	base06: "light_foreground",
	base07: "bright_foreground",
	base08: "red",
	base09: "orange",
	base0A: "yellow",
	base0B: "green",
	base0C: "cyan",
	base0D: "blue",
	base0E: "magenta",
	base0F: "brown",
	base10: "dark_background",
	base11: "darker_background",
	base12: "bright_red",
	base13: "bright_yellow",
	base14: "bright_green",
	base15: "bright_cyan",
	base16: "bright_blue",
	base17: "bright_magenta",
};

/** Legacy ANSI `color0..color15` fallback (best-effort, per ADR-0040). */
const LEGACY_ANSI = {
	base00: "color0",
	base03: "color8",
	base05: "color7",
	base07: "color15",
	base08: "color1",
	base0A: "color3",
	base0B: "color2",
	base0C: "color6",
	base0D: "color4",
	base0E: "color5",
	base12: "color9",
	base13: "color11",
	base14: "color10",
	base15: "color14",
	base16: "color12",
	base17: "color13",
};

function resolveSlots(colors) {
	const slots = {};
	let legacy = false;
	for (const [slot, key] of Object.entries(CANONICAL)) {
		if (colors[key]) slots[slot] = colors[key];
	}
	if (Object.keys(slots).length === 0) {
		legacy = true;
		for (const [slot, key] of Object.entries(LEGACY_ANSI)) {
			if (colors[key]) slots[slot] = colors[key];
		}
	}
	return { slots, legacy };
}

function render({ source, mode, accent, slots, legacy }) {
	const slotLines = Object.entries(slots)
		.map(([slot, value]) => `\t--${slot}: ${value};`)
		.join("\n");
	const header = [
		"/* Omarchy theme adapter output — machine-local, DO NOT EDIT BY HAND.",
		" * Regenerate with `just omarchy` (scripts/omarchy-theme.mjs).",
		` * Source: ${source}`,
		` * Mode: ${mode}${accent ? ` · Accent: ${accent}` : ""}${legacy ? " · legacy ANSI palette (partial)" : ""}`,
		" */",
		":root {",
		`\t--omarchy-mode: ${mode};`,
		`\t--omarchy-accent: ${accent ?? "var(--primary)"};`,
		slotLines,
		"}",
		"",
	];
	return header.join("\n");
}

const PASSTHROUGH = [
	"/* Omarchy theme adapter output — machine-local, DO NOT EDIT BY HAND.",
	" * No active omarchy theme was found on this machine, so this file is a",
	" * no-op passthrough and the committed house scheme (oqto-dark) applies.",
	" * Run `just omarchy` on an omarchy machine to generate real overrides.",
	" */",
	"",
].join("\n");

function themeName(file) {
	try {
		// current/theme/colors.toml is usually a symlink into themes/<name>/
		return path.basename(path.dirname(realpathSync(file)));
	} catch {
		return path.basename(path.dirname(path.dirname(file)));
	}
}

function generate() {
	const file = findThemeFile();
	if (!file) {
		writeFileSync(OUT_FILE, PASSTHROUGH);
		console.log(`omarchy: no theme found — wrote passthrough to ${path.relative(process.cwd(), OUT_FILE)}`);
		return;
	}
	const colors = parseColorsToml(readFileSync(file, "utf8"));
	const { slots, legacy } = resolveSlots(colors);
	if (Object.keys(slots).length === 0) {
		writeFileSync(OUT_FILE, PASSTHROUGH);
		console.log(`omarchy: ${file} has no usable color keys — wrote passthrough`);
		return;
	}
	const mode = colors.mode ?? colors.theme_type ?? (/\blight\b/.test(file) ? "light" : "dark");
	const css = render({
		source: file,
		mode,
		accent: colors.accent,
		slots,
		legacy,
	});
	mkdirSync(path.dirname(OUT_FILE), { recursive: true });
	writeFileSync(OUT_FILE, css);
	console.log(`omarchy: applied ${themeName(file)} (${mode}) → ${path.relative(process.cwd(), OUT_FILE)}`);
}

generate();

if (process.argv.includes("--watch")) {
	const file = findThemeFile();
	if (file) {
		// colors.toml is replaced atomically on theme switch — watch its dir.
		const dir = path.dirname(file);
		let last = readFileSync(file, "utf8");
		setInterval(() => {
			try {
				const current = readFileSync(file, "utf8");
				if (current !== last) {
					last = current;
					generate();
				}
			} catch {}
		}, 2000);
		console.log(`omarchy: watching ${dir} (poll)`);
	} else {
		console.log("omarchy: nothing to watch (no theme file)");
	}
}
