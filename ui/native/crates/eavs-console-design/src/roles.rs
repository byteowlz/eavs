//! The closed abstract role layer — the portable surface (spec/roles.md).
//! A role is a semantic intent bound to a slot. Multiple roles may bind the
//! same slot: hue and role are separate indirections.

use crate::color::Rgba;
use crate::scheme::Scheme;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    // Foreground ramp (2 levels).
    Foreground,
    MutedForeground,
    // Surface depth (3 levels).
    Background,
    Surface,
    SurfaceSunken,
    // Action & status.
    Primary,
    Secondary,
    Muted,
    Accent,
    Success,
    Warning,
    Danger,
    Info,
    // Structure.
    Border,
    Ring,
    Input,
}

impl Role {
    pub const ALL: [Role; 16] = [
        Role::Foreground,
        Role::MutedForeground,
        Role::Background,
        Role::Surface,
        Role::SurfaceSunken,
        Role::Primary,
        Role::Secondary,
        Role::Muted,
        Role::Accent,
        Role::Success,
        Role::Warning,
        Role::Danger,
        Role::Info,
        Role::Border,
        Role::Ring,
        Role::Input,
    ];

    /// Default role -> slot binding (spec/roles.md).
    pub fn slot(self) -> &'static str {
        match self {
            Role::Foreground => "base05",
            Role::MutedForeground => "base04",
            Role::Background => "base00",
            Role::Surface => "base01",
            Role::SurfaceSunken => "base11",
            Role::Primary => "base0B",
            Role::Secondary => "base02",
            Role::Muted => "base02",
            Role::Accent => "base02",
            Role::Success => "base0B",
            Role::Warning => "base0A",
            Role::Danger => "base08",
            Role::Info => "base0D",
            Role::Border => "base01",
            Role::Ring => "base0B",
            Role::Input => "base01",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Role::Foreground => "foreground",
            Role::MutedForeground => "muted-foreground",
            Role::Background => "background",
            Role::Surface => "surface",
            Role::SurfaceSunken => "surface-sunken",
            Role::Primary => "primary",
            Role::Secondary => "secondary",
            Role::Muted => "muted",
            Role::Accent => "accent",
            Role::Success => "success",
            Role::Warning => "warning",
            Role::Danger => "danger",
            Role::Info => "info",
            Role::Border => "border",
            Role::Ring => "ring",
            Role::Input => "input",
        }
    }
}

/// Resolved portable roles for one scheme.
#[derive(Debug, Clone)]
pub struct Roles<'a> {
    scheme: &'a Scheme,
}

impl<'a> Roles<'a> {
    pub fn new(scheme: &'a Scheme) -> Self {
        Self { scheme }
    }

    pub fn get(&self, role: Role) -> Rgba {
        self.scheme.slot(role.slot())
    }

    pub fn scheme(&self) -> &'a Scheme {
        self.scheme
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_names_and_slots_cover_closed_set() {
        let mut names: Vec<&'static str> = Role::ALL.iter().map(|r| r.name()).collect();
        names.sort_unstable();
        let mut expect = vec![
            "accent",
            "background",
            "border",
            "danger",
            "foreground",
            "info",
            "input",
            "muted",
            "muted-foreground",
            "primary",
            "ring",
            "secondary",
            "success",
            "surface",
            "surface-sunken",
            "warning",
        ];
        expect.sort_unstable();
        assert_eq!(names, expect);

        // primary/success share a slot; secondary/muted/accent share a slot.
        assert_eq!(Role::Primary.slot(), Role::Success.slot());
        assert_eq!(Role::Secondary.slot(), Role::Muted.slot());
        assert_eq!(Role::Muted.slot(), Role::Accent.slot());
    }
}
