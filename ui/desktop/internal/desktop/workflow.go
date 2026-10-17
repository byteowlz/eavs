package desktop

import (
	"context"
	"fmt"

	"github.com/byteowlz/eavs/desktop/control"
	"github.com/egoist/mygo/ui"
)

// Workflow state stays on the UI thread. Approval binds immutable form values,
// both Rust revision pins and a fresh scoped server observation at apply time.
func (a *App) beginWorkflow(apply bool) {
	if a.closed.Load() || a.Update == nil || a.Writer == nil || a.WriteBusy || a.Model.Busy {
		return
	}
	kind := a.Panel
	draft := a.Draft
	target := a.PiTarget
	if (kind == "enroll" && !draft.Valid()) || (kind == "pi" && !target.Valid()) || (kind != "enroll" && kind != "pi") {
		return
	}
	var approved *control.Change
	if apply {
		if a.Pending == nil || a.pendingKind != kind || (kind == "enroll" && draft != a.pendingDraft) || (kind == "pi" && target != a.pendingTarget) {
			return
		}
		copy := *a.Pending
		approved = &copy
	}
	a.Pending = nil
	a.WriteBusy = true
	a.WriteMessage = "Reading current scope and preparing the explicit change…"
	a.writeGeneration++
	generation := a.writeGeneration
	ctx, cancel := context.WithCancel(context.Background())
	a.writeCancel = cancel
	go func() {
		defer cancel()
		var result control.Change
		var err error
		if kind == "enroll" {
			result, err = a.Writer.Enrollment(ctx, draft, approved)
		} else {
			result, err = a.Writer.Pi(ctx, target, approved)
		}
		if a.closed.Load() {
			return
		}
		a.Update(func() {
			if a.closed.Load() || generation != a.writeGeneration {
				return
			}
			a.WriteBusy = false
			if err != nil {
				a.WriteMessage = "Operation or reply unavailable. Inspect current state and preview again; nothing is retried automatically."
				return
			}
			if apply {
				a.WriteMessage = "Applied. Refresh connections or reopen Pi’s model picker to inspect the result."
				return
			}
			a.Pending = &result
			a.pendingKind = kind
			a.pendingDraft = draft
			a.pendingTarget = target
			a.WriteMessage = "Preview ready. Confirm below to record this explicitly trusted gateway and its local credential references."
			if kind == "pi" {
				a.WriteMessage = fmt.Sprintf("Preview ready: %d managed providers, %d unsupported/unpriced models omitted. Unrelated configuration is preserved. Prices are unverified estimates.", len(result.ManagedProviders), result.SkippedModels)
			}
		})
	}()
}
func (a *App) dismissWorkflow() {
	if a.writeCancel != nil {
		a.writeCancel()
	}
	a.writeGeneration++
	a.Pending = nil
	a.WriteBusy = false
	a.Panel = ""
	a.WriteMessage = ""
}
func (a *App) workflowView(c *ui.Context) {
	ui.Scroll(c).Grow(1).Children(func() {
		ui.Column(c).MaxWidth(760).Gap(12).Children(func() {
			if a.Panel == "enroll" {
				ui.Text(c, "Connect this device").FontSize(22).Bold().Role(ui.RoleHeading).Level(2)
				ui.Text(c, "Use an already-issued scoped virtual key in a machine-local environment variable. Enter variable names below, never secret values. This does not issue a key or copy provider credentials.")
				ui.Text(c, "Connection name")
				ui.TextInput(c.Key("name"), &a.Draft.Name).Label("Connection name").Placeholder("central").Disabled(a.WriteBusy)
				ui.Text(c, "Gateway URL")
				if ui.TextInput(c.Key("url"), &a.Draft.URL).Label("Gateway URL").Placeholder("https://gateway.example.test").Disabled(a.WriteBusy).Changed() {
					a.Draft.Trust = false
					a.Pending = nil
				}
				ui.Text(c, "Scoped-key environment variable")
				if ui.TextInput(c.Key("key-ref"), &a.Draft.KeyEnv).Label("Scoped-key environment variable").Placeholder("EAVS_API_KEY").Disabled(a.WriteBusy).Changed() {
					a.Draft.Trust = false
					a.Pending = nil
				}
				ui.Text(c, "Listener-key environment variable (optional)")
				if ui.TextInput(c.Key("listener-ref"), &a.Draft.ListenerKeyEnv).Label("Listener-key environment variable (optional)").Disabled(a.WriteBusy).Changed() {
					a.Draft.Trust = false
					a.Pending = nil
				}
				ui.Checkbox(c.Key("trust"), &a.Draft.Trust, "I explicitly trust this gateway to receive the scoped key").Disabled(a.WriteBusy)
				ui.Checkbox(c.Key("loopback"), &a.Draft.AllowLoopbackHTTP, "Allow plaintext HTTP to a literal loopback address only").Disabled(a.WriteBusy)
			} else {
				ui.Text(c, "Connect Pi").FontSize(22).Bold().Role(ui.RoleHeading).Level(2)
				ui.Text(c, "Connection: "+a.PiTarget.Name)
				ui.Text(c, "Choose an explicit absolute models.json path. Preview reads the current gateway scope; apply rechecks scope and revisions. Existing unrelated providers are not replaced or executed.")
				ui.Text(c, "Absolute Pi models.json path")
				ui.TextInput(c.Key("pi-path"), &a.PiTarget.Output).Label("Absolute Pi models.json path").Placeholder("/home/you/.pi/agent/models.json").Disabled(a.WriteBusy)
			}
			if a.WriteMessage != "" {
				ui.Text(c, a.WriteMessage)
			}
			valid := a.Panel == "enroll" && a.Draft.Valid() || a.Panel == "pi" && a.PiTarget.Valid()
			approved := a.Pending != nil && a.pendingKind == a.Panel && (a.Panel == "enroll" && a.Draft == a.pendingDraft || a.Panel == "pi" && a.PiTarget == a.pendingTarget)
			ui.Row(c).Wrap().Gap(12).Children(func() {
				if ui.Button(c.Key("preview"), "Preview change").MinHeight(44).Disabled(!valid || a.WriteBusy || a.Model.Busy || a.Writer == nil).Clicked() {
					a.beginWorkflow(false)
				}
				if ui.Button(c.Key("apply"), "Confirm and apply").MinHeight(44).Disabled(!approved || a.WriteBusy || a.Model.Busy).Clicked() {
					a.beginWorkflow(true)
				}
				if ui.Button(c.Key("cancel"), "Back / cancel").MinHeight(44).Clicked() {
					a.dismissWorkflow()
				}
			})
			ui.Text(c, "Cancelling an apply can lose its receipt after publication. Inspect current state before retrying. No inference or automatic fallback is performed.")
		})
	})
}
