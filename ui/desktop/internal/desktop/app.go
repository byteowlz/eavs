package desktop

import (
	"context"
	"fmt"
	"strings"
	"sync/atomic"

	design "github.com/byteowlz/design-system/impls/mygo-go"
	"github.com/byteowlz/eavs/desktop/control"
	"github.com/egoist/mygo/ui"
)

// App stores model data, never ui.Elements. Update dispatches to Window.Update.
type App struct {
	Runner          control.Runner
	Model           control.Presenter
	Design          design.Resolved
	Update          func(func())
	Query           string
	cancel          context.CancelFunc
	closed          atomic.Bool
	announced       string
	Writer          control.Workflow
	Panel           string
	Draft           control.Enrollment
	PiTarget        control.PiTarget
	Pending         *control.Change
	WriteBusy       bool
	WriteMessage    string
	pendingKind     string
	pendingDraft    control.Enrollment
	pendingTarget   control.PiTarget
	writeGeneration uint64
	writeCancel     context.CancelFunc
}

func New(runner control.Runner, theme design.Resolved) *App {
	app := &App{Runner: runner, Model: control.NewPresenter(), Design: theme}
	if writer, ok := runner.(control.Workflow); ok {
		app.Writer = writer
	}
	return app
}

// Refresh runs only the explicitly requested shared metadata operation.
func (a *App) Refresh() {
	if a.closed.Load() || a.Update == nil || a.WriteBusy {
		return
	}
	if a.cancel != nil {
		a.cancel()
	}
	ctx, cancel := context.WithCancel(context.Background())
	a.cancel = cancel
	generation := a.Model.Begin()
	go func() {
		result, err := a.Runner.Status(ctx)
		if a.closed.Load() {
			return
		}
		a.Update(func() {
			if !a.closed.Load() {
				a.Model.Complete(generation, result, err)
			}
		})
	}()
}

// Close is called on the UI thread, cancelling work before releasing the model.
func (a *App) Close() {
	a.closed.Store(true)
	if a.cancel != nil {
		a.cancel()
	}
	if a.writeCancel != nil {
		a.writeCancel()
	}
	a.Model.Close()
}
func (a *App) View(c *ui.Context) {
	message := a.Model.Message + " " + a.WriteMessage
	if a.announced != message {
		c.Announce(message)
		a.announced = message
	}
	theme := a.Design.Theme
	theme.FontSize *= c.Preferences().TextScale
	c.SetTheme(&theme)
	ui.Column(c).Fill().Padding(24).Gap(16).Children(func() {
		ui.Row(c).Wrap().Gap(12).Children(func() {
			ui.Column(c).Grow(1).Gap(4).Children(func() {
				ui.Text(c, "EAVS / Connection Desk").FontSize(24).Bold().Role(ui.RoleHeading).Level(1)
				ui.Text(c, "Your routes. Their owners. No hidden fallback.").TextColor(a.Design.Roles[design.MutedForeground])
			})
			if ui.Button(c.Key("enroll"), "Connect device").MinHeight(44).Disabled(a.Model.Busy || a.WriteBusy || a.Writer == nil).Clicked() {
				a.dismissWorkflow()
				a.Panel = "enroll"
				a.Draft.Trust = false
			}
			if ui.Button(c.Key("pi"), "Connect Pi").MinHeight(44).Disabled(a.Model.Busy || a.WriteBusy || a.Writer == nil || a.Model.Connection() == nil).Clicked() {
				a.dismissWorkflow()
				a.Panel = "pi"
				a.PiTarget = control.PiTarget{Name: a.Model.Selected}
			}
			if ui.Button(c.Key("refresh"), "Refresh connections").MinHeight(44).Disabled(a.Model.Busy || a.WriteBusy).Clicked() {
				a.Refresh()
				c.Announce("Reading connection metadata")
			}
		})
		ui.Column(c).Padding(12).Radius(a.Design.Radius.MD).Background(a.Design.Roles[design.SurfaceSunken]).Gap(4).Children(func() {
			ui.Text(c, a.Model.Message)
			if a.Model.Stale {
				ui.Text(c, "LAST KNOWN / NOT CURRENT").Bold().TextColor(a.Design.Roles[design.Warning])
			}
		})
		if a.Panel != "" {
			a.workflowView(c)
		} else {
			ui.Row(c).Wrap().Gap(16).Grow(1).Children(func() { a.connections(c); a.detail(c) })
		}
		ui.Text(c, "Explicit preview/confirmation · metadata reachability is not upstream health · credentials stay with their owner").TextColor(a.Design.Roles[design.MutedForeground])
	})
}
func (a *App) connections(c *ui.Context) {
	ui.Column(c).Width(260).MinHeight(200).Gap(12).Children(func() {
		ui.Text(c, "CONNECTIONS").Bold().Role(ui.RoleHeading).Level(2)
		if len(a.Model.Snapshot.Connections) == 0 {
			ui.Text(c, "No connections loaded.")
			ui.Text(c, "Choose Connect device (or use the eavs CLI), then refresh. Nothing is discovered or trusted silently.").TextColor(a.Design.Roles[design.MutedForeground])
		}
		ui.Scroll(c).Grow(1).Children(func() {
			for _, connection := range a.Model.Snapshot.Connections {
				ui.Column(c.Key("connection:" + connection.Name)).Padding(12).Gap(4).Radius(a.Design.Radius.MD).Background(a.Design.Roles[design.SurfaceSunken]).Children(func() {
					if ui.Button(c.Key("select"), connection.Name).MinHeight(44).Clicked() {
						a.Model.Select(connection.Name)
						c.Announce("Selected connection " + connection.Name)
					}
					ui.Text(c, control.StateLabel(connection.State))
				})
			}
		})
	})
}
func (a *App) detail(c *ui.Context) {
	ui.Column(c).Grow(1).MinWidth(280).Gap(12).Children(func() {
		connection := a.Model.Connection()
		if connection == nil {
			ui.Text(c, "Choose a connection").FontSize(20).Bold().Role(ui.RoleHeading).Level(2)
			ui.Text(c, "No connection is selected automatically. Server scopes determine which model candidates you may discover; this screen grants no access.")
			ui.Text(c, "Headless equivalent: eavs device status --json").TextColor(a.Design.Roles[design.MutedForeground])
			return
		}
		ui.Text(c, connection.Name).FontSize(20).Bold().Role(ui.RoleHeading).Level(2)
		ui.Text(c, connection.URL)
		ui.Text(c, control.StateLabel(connection.State)).Bold()
		if message := control.DiagnosticLabel(connection.Diagnostic); message != "" {
			ui.Text(c, message).TextColor(a.Design.Roles[design.MutedForeground])
		}
		if connection.State != "ready" {
			ui.Text(c, "This connection cannot currently supply fresh metadata. Routes are not deleted, migrated or retried through another billing arrangement.")
		}
		ui.TextInput(c.Key("filter"), &a.Query).Placeholder("Filter discovered models").Label("Filter discovered models")
		ui.Scroll(c).Grow(1).Children(func() {
			for _, provider := range connection.Providers {
				ui.Column(c.Key("provider:" + provider.ID)).Padding(12).Gap(8).Radius(a.Design.Radius.MD).Background(a.Design.Roles[design.SurfaceSunken]).Children(func() {
					ui.Text(c, provider.ID).Bold().Role(ui.RoleHeading).Level(3)
					protocol := provider.API
					if protocol == "" {
						protocol = "Scoped catalog"
					}
					ui.Text(c, fmt.Sprintf("%s / %d candidate models", protocol, len(provider.Models))).TextColor(a.Design.Roles[design.MutedForeground])
					for _, model := range provider.Models {
						if a.Query != "" && !strings.Contains(strings.ToLower(model.ID+" "+model.Name), strings.ToLower(a.Query)) {
							continue
						}
						ui.Text(c.Key("model:"+model.ID), model.ID+" — "+model.Name)
					}
				})
			}
		})
	})
}
