package design

import (
	"encoding/json"
	"fmt"
	"math"

	"github.com/egoist/mygo/ui"
)

type Role string

const (
	Background      Role = "background"
	Surface         Role = "surface"
	SurfaceSunken   Role = "surface-sunken"
	Foreground      Role = "foreground"
	MutedForeground Role = "muted-foreground"
	Primary         Role = "primary"
	Secondary       Role = "secondary"
	Muted           Role = "muted"
	Accent          Role = "accent"
	Success         Role = "success"
	Warning         Role = "warning"
	Danger          Role = "danger"
	Info            Role = "info"
	Border          Role = "border"
	Ring            Role = "ring"
	Input           Role = "input"
)

type RadiusScale struct{ SM, MD, LG, XL float32 }

func Radius(dial float32) (RadiusScale, error) {
	if dial < 0 || math.IsNaN(float64(dial)) || math.IsInf(float64(dial), 0) {
		return RadiusScale{}, fmt.Errorf("radius must be finite and nonnegative")
	}
	scale := RadiusScale{dial * 0.5, dial * 0.75, dial, dial * 1.25}
	if math.IsInf(float64(scale.XL), 0) {
		return RadiusScale{}, fmt.Errorf("computed radius tiers must remain finite")
	}
	return scale, nil
}

type Resolved struct {
	Roles  map[Role]ui.Color
	Slots  map[string]string
	Radius RadiusScale
	Theme  ui.Theme
}

// Resolve fills a theme entirely from roles; no toolkit default palette is copied.
// Native widget vocabulary stays separate from the closed portable role table.
func Resolve(scheme Scheme, dial float32) (Resolved, error) {
	slots, err := normalize(scheme)
	if err != nil {
		return Resolved{}, err
	}
	radius, err := Radius(dial)
	if err != nil {
		return Resolved{}, err
	}
	raw, err := data.ReadFile("data/roles.json")
	if err != nil {
		return Resolved{}, err
	}
	var bindings map[Role]string
	if err := json.Unmarshal(raw, &bindings); err != nil {
		return Resolved{}, err
	}
	resolved := Resolved{Roles: make(map[Role]ui.Color, 16), Slots: make(map[string]string, 24), Radius: radius}
	for key, value := range slots {
		resolved.Slots[key] = value.hex()
	}
	for role, slot := range bindings {
		value := slots[slot]
		if override, ok := scheme.Overrides["--"+string(role)]; ok {
			value, err = parseColor(override)
			if err != nil {
				return Resolved{}, fmt.Errorf("role override: %w", err)
			}
		}
		resolved.Roles[role] = value.native()
	}
	r := resolved.Roles
	resolved.Theme = ui.Theme{
		Dark: scheme.Mode == "dark", Background: r[Background],
		Surface: r[Surface], SurfaceHover: r[Accent], SurfacePressed: r[Secondary],
		Border: r[Border], Text: r[Foreground], TextMuted: r[MutedForeground],
		Accent: r[Primary], AccentHover: r[Primary], AccentPressed: r[Primary],
		AccentText: r[Background], Danger: r[Danger], Warning: r[Warning], Success: r[Success],
		Selection: r[Accent], Focus: r[Ring],
		Inverse: r[SurfaceSunken], InverseText: r[Foreground], Scrollbar: r[MutedForeground],
		ScrollbarWidth: 6, Radius: radius.SM, Spacing: 4, FontSize: 14,
	}
	return resolved, nil
}
