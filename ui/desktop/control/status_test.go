package control

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"
)

const fixture = `{"schema_version":1,"connections":[{"name":"home","url":"https://eavs.example.test","state":"ready","providers":[{"id":"alpha","api":"openai-completions","models":[{"id":"model-a","name":"Model A"}]}]}]}`

func sample(t *testing.T) Snapshot {
	t.Helper()
	value, err := Decode([]byte(fixture))
	if err != nil {
		t.Fatal(err)
	}
	return value
}
func TestDecodeClosedSafeProjection(t *testing.T) {
	value := sample(t)
	if value.Connections[0].Providers[0].Models[0].Name != "Model A" {
		t.Fatal("model projection lost")
	}
	for _, input := range []string{
		`null`, `[]`, `{"schema_version":2,"connections":[]}`, `{"schema_version":1}`, fixture + `{}`,
		strings.Replace(fixture, `"ready"`, `"anything"`, 1),
		strings.Replace(fixture, `https://eavs.example.test`, `https://secret@eavs.example.test`, 1),
		strings.Replace(fixture, `https://eavs.example.test`, `https://eavs.example.test?token=secret`, 1),
		strings.Replace(fixture, `https://eavs.example.test`, `http://eavs.example.test`, 1),
		strings.Replace(fixture, `"home"`, `"home\u001b[31m"`, 1),
		strings.Replace(fixture, `"Model A"`, `"bad\nlabel"`, 1),
		strings.Repeat(" ", MaxReply+1),
	} {
		if _, err := Decode([]byte(input)); !errors.Is(err, ErrInvalid) {
			t.Fatal("invalid protocol accepted")
		}
	}
	value, err := Decode([]byte(strings.Replace(fixture, `"schema_version":1`, `"secret":"private","schema_version":1`, 1)))
	if err != nil || value.SchemaVersion != 1 {
		t.Fatal("safe future fields were not projected")
	}
}
func TestPresenterRejectsStaleAndClosedResults(t *testing.T) {
	p := NewPresenter()
	first := p.Begin()
	second := p.Begin()
	if p.Complete(first, sample(t), nil) {
		t.Fatal("stale callback accepted")
	}
	value := sample(t)
	if !p.Complete(second, value, nil) || p.Stale || p.Selected != "" {
		t.Fatal("fresh result or explicit selection invariant")
	}
	value.Connections[0].Providers[0].Models[0].Name = "mutated"
	if p.Snapshot.Connections[0].Providers[0].Models[0].Name != "Model A" {
		t.Fatal("worker mutated presenter data")
	}
	if !p.Select("home") || p.Select("foreign") {
		t.Fatal("selection boundary")
	}
	copy := p.Connection()
	copy.Providers[0].Models[0].Name = "mutated"
	if p.Connection().Providers[0].Models[0].Name != "Model A" {
		t.Fatal("view mutated presenter")
	}
	third := p.Begin()
	p.Complete(third, Snapshot{}, errors.New("secret"))
	if !p.Stale || strings.Contains(p.Message, "secret") || p.Connection() == nil {
		t.Fatal("stale private failure not marked")
	}
	last := p.Begin()
	p.Close()
	if p.Complete(last, sample(t), nil) || p.Begin() != 0 {
		t.Fatal("closed presenter changed")
	}
}
func TestSelectionClearsOnConnectionRemoval(t *testing.T) {
	p := NewPresenter()
	p.Complete(p.Begin(), sample(t), nil)
	p.Select("home")
	p.Complete(p.Begin(), Snapshot{SchemaVersion: 1, Connections: []Connection{}}, nil)
	if p.Selected != "" || p.Connection() != nil {
		t.Fatal("removed selection retained")
	}
}
func executable(t *testing.T, body string) string {
	t.Helper()
	if runtime.GOOS == "windows" {
		t.Skip("POSIX fake executable; decode/presenter tests remain portable")
	}
	path := filepath.Join(t.TempDir(), "fake-eavs")
	if err := os.WriteFile(path, []byte("#!/bin/sh\n"+body), 0700); err != nil {
		t.Fatal(err)
	}
	return path
}
func TestExecUsesExactReadOnlyArgumentsAndPrivateErrors(t *testing.T) {
	path := executable(t, "[ \"$#\" = 3 ] && [ \"$1\" = device ] && [ \"$2\" = status ] && [ \"$3\" = --json ] || exit 7\nprintf '%s' '"+fixture+"'\n")
	value, err := (ExecRunner{Binary: path}).Status(context.Background())
	if err != nil || len(value.Connections) != 1 {
		t.Fatal("shared CLI not consumed", err)
	}
	for _, body := range []string{"echo private-secret >&2; exit 1", "printf '%s' 'private-secret'", "printf '%s' '{\"schema_version\":1,\"connections\":[]}' ; printf extra"} {
		_, err := (ExecRunner{Binary: executable(t, body)}).Status(context.Background())
		if err == nil || strings.Contains(err.Error(), "private-secret") {
			t.Fatal("invalid/private child output leaked")
		}
	}
}
func TestExecCancelledAndBounded(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if _, err := (ExecRunner{Binary: executable(t, "exit 0")}).Status(ctx); !errors.Is(err, ErrUnavailable) {
		t.Fatal("cancel ignored")
	}
	start := time.Now()
	if _, err := (ExecRunner{Binary: executable(t, "exec sleep 10"), Timeout: 20 * time.Millisecond}).Status(context.Background()); !errors.Is(err, ErrUnavailable) || time.Since(start) > 2*time.Second {
		t.Fatal("timeout not bounded")
	}
	b := bounded{Limit: 2}
	if _, err := b.Write([]byte("123")); err == nil || b.Len() != 0 {
		t.Fatal("output bound ignored")
	}
}
func TestDiagnosticProjectionDoesNotDisplayUnknownPrivateText(t *testing.T) {
	if strings.Contains(DiagnosticLabel("private-secret"), "private-secret") {
		t.Fatal("unrecognized diagnostic leaked")
	}
	if DiagnosticLabel("rate_limited") == DiagnosticLabel("server_failure") || DiagnosticLabel("transport_timeout") == DiagnosticLabel("authentication_rejected") {
		t.Fatal("distinct failures collapsed")
	}
}

func TestStatusLabelsAreExplicit(t *testing.T) {
	for _, state := range []string{"ready", "unreachable", "unauthorized", "unavailable", "unconfigured", "unknown"} {
		if StateLabel(state) == "" {
			t.Fatal("missing noncolor status")
		}
	}
}
