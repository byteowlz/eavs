// Package control adapts the shared Rust device operations, not inference policy.
package control

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/url"
	"os/exec"
	"strings"
	"time"
	"unicode"
)

const MaxReply = 4 * 1024 * 1024

var (
	ErrUnavailable = errors.New("device operation unavailable; inspect current state and preview again before retrying")
	ErrInvalid     = errors.New("device operation did not match the supported public protocol")
)

type Model struct {
	ID   string `json:"id"`
	Name string `json:"name"`
}
type Provider struct {
	ID     string  `json:"id"`
	API    string  `json:"api"`
	Models []Model `json:"models"`
}
type Connection struct {
	Name       string     `json:"name"`
	URL        string     `json:"url"`
	State      string     `json:"state"`
	Diagnostic string     `json:"diagnostic"`
	Providers  []Provider `json:"providers"`
}
type Snapshot struct {
	SchemaVersion int          `json:"schema_version"`
	Connections   []Connection `json:"connections"`
}

func clean(value string, max int) bool {
	return len(value) <= max && !strings.ContainsFunc(value, unicode.IsControl)
}
func endpoint(value string) bool {
	u, err := url.Parse(value)
	return err == nil && (u.Scheme == "https" || (u.Scheme == "http" && (u.Hostname() == "localhost" || u.Hostname() == "127.0.0.1" || u.Hostname() == "::1"))) && u.Host != "" && u.User == nil && u.RawQuery == "" && u.Fragment == "" && clean(value, 2048)
}
func validState(state string) bool {
	switch state {
	case "ready", "unreachable", "unauthorized", "unavailable", "unconfigured":
		return true
	}
	return false
}

// Decode projects only known public fields. Unknown future fields are never displayed.
func Decode(data []byte) (Snapshot, error) {
	var result Snapshot
	if len(data) > MaxReply {
		return result, ErrInvalid
	}
	decoder := json.NewDecoder(bytes.NewReader(data))
	if decoder.Decode(&result) != nil {
		return Snapshot{}, ErrInvalid
	}
	var trailing any
	if decoder.Decode(&trailing) != io.EOF || result.SchemaVersion != 1 || result.Connections == nil || len(result.Connections) > 256 {
		return Snapshot{}, ErrInvalid
	}
	names := make(map[string]bool)
	models := 0
	for _, connection := range result.Connections {
		if connection.Name == "" || !clean(connection.Name, 128) || names[connection.Name] || !endpoint(connection.URL) || !validState(connection.State) || !clean(connection.Diagnostic, 128) || len(connection.Providers) > 256 {
			return Snapshot{}, ErrInvalid
		}
		names[connection.Name] = true
		providers := make(map[string]bool)
		for _, provider := range connection.Providers {
			if provider.ID == "" || providers[provider.ID] || !clean(provider.ID, 128) || !clean(provider.API, 128) {
				return Snapshot{}, ErrInvalid
			}
			providers[provider.ID] = true
			ids := make(map[string]bool)
			for _, model := range provider.Models {
				models++
				if models > 4096 || model.ID == "" || ids[model.ID] || !clean(model.ID, 512) || !clean(model.Name, 512) {
					return Snapshot{}, ErrInvalid
				}
				ids[model.ID] = true
			}
		}
	}
	return result, nil
}

// Runner reads status. Workflow is an optional explicit preview/apply capability.
type Runner interface {
	Status(context.Context) (Snapshot, error)
}
type ExecRunner struct {
	Binary  string
	Timeout time.Duration
}
type bounded struct {
	bytes.Buffer
	Limit int
}

func (b *bounded) Write(value []byte) (int, error) {
	if len(value) > b.Limit-b.Len() {
		return 0, ErrUnavailable
	}
	return b.Buffer.Write(value)
}
func (runner ExecRunner) Status(ctx context.Context) (Snapshot, error) {
	output, err := runner.invoke(ctx, []string{"device", "status", "--json"})
	if err != nil {
		return Snapshot{}, err
	}
	return Decode(output)
}
func (runner ExecRunner) invoke(ctx context.Context, args []string) ([]byte, error) {
	if runner.Binary == "" {
		return nil, ErrUnavailable
	}
	timeout := runner.Timeout
	if timeout <= 0 || timeout > 30*time.Second {
		timeout = 15 * time.Second
	}
	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()
	// Never use a shell or caller-supplied argument list. Stderr may contain private data.
	command := exec.CommandContext(ctx, runner.Binary, args...)
	output := &bounded{Limit: MaxReply}
	command.Stdout = output
	command.Stderr = io.Discard
	command.WaitDelay = time.Second
	if command.Run() != nil {
		return nil, ErrUnavailable
	}
	return output.Bytes(), nil
}

func StateLabel(state string) string {
	switch state {
	case "ready":
		return "Metadata reachable"
	case "unreachable":
		return "Network unreachable"
	case "unauthorized":
		return "Authentication required"
	case "unavailable":
		return "Service unavailable"
	case "unconfigured":
		return "Not configured"
	default:
		return "Unknown — refresh required"
	}
}
