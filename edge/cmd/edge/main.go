package main

import (
	"log"
	"net/http"
	"os"

	"sandbox.local/edge"
)

func main() {
	p := &edge.Proxy{
		APIURL:        os.Getenv("API_URL"),
		InternalToken: os.Getenv("INTERNAL_TOKEN"),
	}
	if p.APIURL == "" {
		log.Fatal("API_URL is required")
	}
	addr := os.Getenv("ADDR")
	if addr == "" {
		addr = ":3002"
	}
	if err := http.ListenAndServe(addr, p.Handler()); err != nil {
		log.Fatal(err)
	}
}
