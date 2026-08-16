package cli

import (
	"context"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"
	"github.com/omendb/fleet/internal/config"
	"github.com/omendb/fleet/internal/logging"
	"github.com/omendb/fleet/internal/server"
	"github.com/rs/zerolog/log"
	"github.com/spf13/cobra"
)

var serveCmd = &cobra.Command{
	Use:   "serve",
	Short: "Start the Fleet control plane and inference gateway server",
	Run: func(cmd *cobra.Command, args []string) {
		// 1. Load config
		cfg, err := config.LoadConfig()
		if err != nil {
			log.Fatal().Err(err).Msg("Failed to load configuration")
		}

		// 2. Initialize logger
		logging.InitLogger(cfg.LogLevel, cfg.Environment)

		log.Info().Msg("Initializing database connection pool...")

		// 3. Connect to database
		dbCtx, dbCancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer dbCancel()

		dbPool, err := pgxpool.New(dbCtx, cfg.DatabaseURL)
		if err != nil {
			log.Fatal().Err(err).Msg("Failed to create database pool")
		}
		defer dbPool.Close()

		// Test connection
		if err := dbPool.Ping(dbCtx); err != nil {
			log.Fatal().Err(err).Msg("Failed to ping database")
		}
		log.Info().Msg("Connected to PostgreSQL successfully")

		// 4. Start Server
		if err := server.StartServer(cfg, dbPool); err != nil {
			log.Fatal().Err(err).Msg("Server exited unexpectedly")
		}
	},
}

func init() {
	RootCmd.AddCommand(serveCmd)
}
