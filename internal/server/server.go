package server

import (
	"context"
	"fmt"
	"net/http"
	"time"

	"github.com/go-chi/chi/v5"
	"github.com/go-chi/chi/v5/middleware"
	"github.com/jackc/pgx/v5/pgxpool"
	"github.com/omendb/fleet/gen/proto/fleet/v1/fleetv1connect"
	"github.com/omendb/fleet/internal/config"
	"github.com/rs/zerolog/log"
	"golang.org/x/net/http2"
	"golang.org/x/net/http2/h2c"

	"connectrpc.com/connect"
	fleetv1 "github.com/omendb/fleet/gen/proto/fleet/v1"
)

// ControlServer implements the ConnectRPC ControlService handler
type ControlServer struct {
	dbPool *pgxpool.Pool
}

func NewControlServer(dbPool *pgxpool.Pool) *ControlServer {
	return &ControlServer{dbPool: dbPool}
}

func (s *ControlServer) RegisterNode(
	ctx context.Context,
	req *connect.Request[fleetv1.RegisterNodeRequest],
) (*connect.Response[fleetv1.RegisterNodeResponse], error) {
	log.Info().Fields(map[string]interface{}{
		"node_id":  req.Msg.NodeId,
		"hostname": req.Msg.Hostname,
	}).Msg("Received node registration request")

	return connect.NewResponse(&fleetv1.RegisterNodeResponse{
		Success:        true,
		Message:        "Node registered successfully",
		HardwarePoolId: "default",
	}), nil
}

func (s *ControlServer) Heartbeat(
	ctx context.Context,
	req *connect.Request[fleetv1.HeartbeatRequest],
) (*connect.Response[fleetv1.HeartbeatResponse], error) {
	log.Debug().Fields(map[string]interface{}{
		"node_id": req.Msg.NodeId,
	}).Msg("Received node heartbeat")

	return connect.NewResponse(&fleetv1.HeartbeatResponse{
		Success: true,
		Action:  "none",
	}), nil
}

func (s *ControlServer) GetWorkloads(
	ctx context.Context,
	req *connect.Request[fleetv1.GetWorkloadsRequest],
) (*connect.Response[fleetv1.GetWorkloadsResponse], error) {
	log.Info().Fields(map[string]interface{}{
		"node_id": req.Msg.NodeId,
	}).Msg("Received workloads fetch request")

	return connect.NewResponse(&fleetv1.GetWorkloadsResponse{
		Workloads: []*fleetv1.Workload{},
	}), nil
}

// StartServer sets up and runs the HTTP and ConnectRPC server
func StartServer(cfg *config.Config, dbPool *pgxpool.Pool) error {
	router := chi.NewRouter()

	// Setup middlewares
	router.Use(middleware.RequestID)
	router.Use(middleware.RealIP)
	router.Use(middleware.Recoverer)
	router.Use(middleware.Timeout(60 * time.Second))

	// Health check endpoint
	router.Get("/health", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(`{"status":"healthy"}`))
	})

	// Register ConnectRPC control service
	controlServer := NewControlServer(dbPool)
	path, handler := fleetv1connect.NewControlServiceHandler(controlServer)
	router.Mount(path, handler)

	addr := fmt.Sprintf(":%d", cfg.Port)
	log.Info().Msgf("Starting server on HTTP address %s", addr)

	// ConnectRPC and gRPC over HTTP require h2c to support HTTP/2 without TLS in local dev
	h2s := &http2.Server{}
	srv := &http.Server{
		Addr:              addr,
		Handler:           h2c.NewHandler(router, h2s),
		ReadHeaderTimeout: 10 * time.Second,
	}

	return srv.ListenAndServe()
}
