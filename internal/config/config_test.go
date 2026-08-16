package config_test

import (
	"os"
	"testing"

	"github.com/omendb/fleet/internal/config"
)

func TestLoadConfig(t *testing.T) {
	// Set test environment variables
	os.Setenv("FLEET_PORT", "9999")
	os.Setenv("FLEET_DATABASE_URL", "postgres://test:test@localhost:5432/testdb")
	defer func() {
		os.Unsetenv("FLEET_PORT")
		os.Unsetenv("FLEET_DATABASE_URL")
	}()

	cfg, err := config.LoadConfig()
	if err != nil {
		t.Fatalf("unexpected error loading config: %v", err)
	}

	if cfg.Port != 9999 {
		t.Errorf("expected port 9999, got %d", cfg.Port)
	}

	if cfg.DatabaseURL != "postgres://test:test@localhost:5432/testdb" {
		t.Errorf("expected database URL to match set env, got %s", cfg.DatabaseURL)
	}
}
