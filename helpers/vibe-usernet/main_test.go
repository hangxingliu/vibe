package main

import (
	"testing"
)

func TestParsePublish(t *testing.T) {
	tests := []struct {
		name        string
		spec        string
		wantLocal   string
		wantRemote  string
		expectError bool
	}{
		{
			name:       "two parts: host_port:guest_port",
			spec:       "8080:80",
			wantLocal:  "0.0.0.0:8080",
			wantRemote: guestIP + ":80",
		},
		{
			name:       "three parts: host_addr:host_port:guest_port",
			spec:       "127.0.0.1:8022:22",
			wantLocal:  "127.0.0.1:8022",
			wantRemote: guestIP + ":22",
		},
		{
			name:       "three parts with 0.0.0.0",
			spec:       "0.0.0.0:3000:3000",
			wantLocal:  "0.0.0.0:3000",
			wantRemote: guestIP + ":3000",
		},
		{
			name:       "udp: two parts: host_port:guest_port",
			spec:       "udp:8080:80",
			wantLocal:  "udp:0.0.0.0:8080",
			wantRemote: guestIP + ":80",
		},
		{
			name:       "udp: three parts: host_addr:host_port:guest_port",
			spec:       "udp:127.0.0.1:8022:22",
			wantLocal:  "udp:127.0.0.1:8022",
			wantRemote: guestIP + ":22",
		},
		{
			name:        "too many parts",
			spec:        "127.0.0.1:8080:80:extra",
			expectError: true,
		},
		{
			name:        "one part",
			spec:        "8080",
			expectError: true,
		},
		{
			name:        "empty",
			spec:        "",
			expectError: true,
		},
		{
			name:       "non-numeric port passes parse (validated at bind time)",
			spec:       "abc:80",
			wantLocal:  "0.0.0.0:abc",
			wantRemote: guestIP + ":80",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			local, remote, err := parsePublish(tt.spec)
			if tt.expectError {
				if err == nil {
					t.Errorf("expected error for spec %q, got local=%q remote=%q", tt.spec, local, remote)
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected error for spec %q: %v", tt.spec, err)
			}
			if local != tt.wantLocal {
				t.Errorf("local: got %q, want %q", local, tt.wantLocal)
			}
			if remote != tt.wantRemote {
				t.Errorf("remote: got %q, want %q", remote, tt.wantRemote)
			}
		})
	}
}

func TestStringSlice(t *testing.T) {
	var s stringSlice
	if got := s.String(); got != "" {
		t.Errorf("empty stringSlice.String() = %q, want \"\"", got)
	}
	if err := s.Set("a"); err != nil {
		t.Fatal(err)
	}
	if err := s.Set("b"); err != nil {
		t.Fatal(err)
	}
	if len(s) != 2 || s[0] != "a" || s[1] != "b" {
		t.Errorf("stringSlice after Set: got %v, want [a, b]", s)
	}
	if got := s.String(); got != "a, b" {
		t.Errorf("stringSlice.String() = %q, want \"a, b\"", got)
	}
}
