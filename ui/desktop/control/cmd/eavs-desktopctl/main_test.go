package main

import (
	"bytes"
	"encoding/json"
	"path/filepath"
	"testing"
)

func TestHelpAndInvalidCommands(t *testing.T) {
	var out, stderr bytes.Buffer
	if run([]string{"--help"}, &out, &stderr) != 0 || out.Len() == 0 || stderr.Len() != 0 {
		t.Fatal("help contract")
	}
	out.Reset()
	stderr.Reset()
	if run([]string{"apply", "--json"}, &out, &stderr) != 2 || out.Len() != 0 || stderr.Len() == 0 {
		t.Fatal("unsupported mutation accepted")
	}
}
func TestUnavailableMachineReplyIsStatic(t *testing.T) {
	var out, stderr bytes.Buffer
	if run([]string{"status", "--json", "--eavs-binary", filepath.Join(t.TempDir(), "missing-private-path")}, &out, &stderr) != 1 {
		t.Fatal("missing binary exit")
	}
	var reply struct {
		Schema int `json:"schema_version"`
		Error  struct {
			Code    string `json:"code"`
			Message string `json:"message"`
		} `json:"error"`
	}
	if json.Unmarshal(out.Bytes(), &reply) != nil || reply.Schema != 1 || reply.Error.Code != "device_status_unavailable" || stderr.Len() != 0 {
		t.Fatal("stable private failure contract")
	}
	if bytes.Contains(out.Bytes(), []byte("missing-private-path")) {
		t.Fatal("private executable path leaked")
	}
}
