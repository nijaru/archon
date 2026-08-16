-- name: GetTenant :one
SELECT * FROM tenants
WHERE id = $1 LIMIT 1;

-- name: CreateTenant :one
INSERT INTO tenants (id, name)
VALUES ($1, $2)
RETURNING *;

-- name: UpsertNode :one
INSERT INTO nodes (id, hardware_pool_id, hostname, ip_address, status, last_heartbeat)
VALUES ($1, $2, $3, $4, $5, $6)
ON CONFLICT (id) DO UPDATE SET
    hardware_pool_id = EXCLUDED.hardware_pool_id,
    hostname = EXCLUDED.hostname,
    ip_address = EXCLUDED.ip_address,
    status = EXCLUDED.status,
    last_heartbeat = EXCLUDED.last_heartbeat
RETURNING *;

-- name: GetNode :one
SELECT * FROM nodes
WHERE id = $1 LIMIT 1;

-- name: UpdateNodeStatus :one
UPDATE nodes
SET status = $2, last_heartbeat = $3
WHERE id = $1
RETURNING *;

-- name: CreateAccelerator :one
INSERT INTO accelerators (id, node_id, vendor, model, vram_mb, uuid)
VALUES ($1, $2, $3, $4, $5, $6)
RETURNING *;

-- name: GetAcceleratorsForNode :many
SELECT * FROM accelerators
WHERE node_id = $1;

-- name: GetEndpoint :one
SELECT * FROM endpoints
WHERE id = $1 LIMIT 1;

-- name: ListEndpoints :many
SELECT * FROM endpoints
WHERE tenant_id = $1
ORDER BY name;
