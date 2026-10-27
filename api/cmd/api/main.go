package main

import (
	"context"
	"log"
	"net/http"
	"os"
	"strings"
	"time"

	"sandbox.local/api/internal/nodeclient"
	"sandbox.local/api/internal/server"
	"sandbox.local/api/internal/store"
)

func main() {
	databaseURL := os.Getenv("DATABASE_URL")
	if databaseURL == "" {
		log.Fatal("DATABASE_URL is required")
	}
	ctx := context.Background()
	pg, err := store.NewPostgres(ctx, databaseURL)
	if err != nil {
		log.Fatal(err)
	}
	defer pg.Close()
	if err := pg.Init(ctx, os.Getenv("BOOTSTRAP_API_KEY")); err != nil {
		log.Fatal(err)
	}
	domain := os.Getenv("SANDBOX_DOMAIN")
	if domain == "" {
		domain = "127.0.0.1"
	}
	var nodes []string
	if raw := os.Getenv("NODE_URLS"); raw != "" {
		for _, p := range strings.Split(raw, ",") {
			p = strings.TrimSpace(p)
			if p != "" {
				nodes = append(nodes, p)
			}
		}
	}
	h := server.New(pg, nodeclient.New(), server.Config{
		InternalToken: os.Getenv("INTERNAL_TOKEN"),
		Domain:        domain,
		NodeURLs:      nodes,
	})
	addr := os.Getenv("ADDR")
	if addr == "" {
		addr = ":8080"
	}
	srv := &http.Server{
		Addr:              addr,
		Handler:           h.Handler(),
		ReadHeaderTimeout: 5 * time.Second,
	}
	log.Printf("api listening on %s", srv.Addr)
	if err := srv.ListenAndServe(); err != nil && err != http.ErrServerClosed {
		log.Fatal(err)
	}
}
