package config

import (
	"fmt"

	"github.com/kelseyhightower/envconfig"
)

// Config holds all config properties loaded from environment variables
type Config struct {
	Port        int    `envconfig:"PORT" default:"8080"`
	DatabaseURL string `envconfig:"DATABASE_URL" default:"postgres://postgres:postgres@localhost:5435/fleet?sslmode=disable"`
	NATSURL     string `envconfig:"NATS_URL" default:"nats://localhost:4222"`
	LogLevel    string `envconfig:"LOG_LEVEL" default:"info"`
	Environment string `envconfig:"ENVIRONMENT" default:"development"`
}

// LoadConfig parses environment variables into a Config struct
func LoadConfig() (*Config, error) {
	var cfg Config
	err := envconfig.Process("FLEET", &cfg)
	if err != nil {
		return nil, fmt.Errorf("failed to process env variables: %w", err)
	}
	return &cfg, nil
}
