.PHONY: build dev test lint migrate-up migrate-down

DB_URL ?= postgres://postgres:postgres@localhost:5435/fleet?sslmode=disable
GOOSE ?= $(shell which goose 2>/dev/null || echo $(HOME)/go/bin/goose)

build:
	go build -o build/fleet ./cmd/fleet

dev: build
	docker compose up -d
	FLEET_ENVIRONMENT=development ./build/fleet serve

migrate-up:
	$(GOOSE) -dir migrations postgres "$(DB_URL)" up

migrate-down:
	$(GOOSE) -dir migrations postgres "$(DB_URL)" down

test:
	go test -v ./...

lint:
	go fmt ./...
	go vet ./...
