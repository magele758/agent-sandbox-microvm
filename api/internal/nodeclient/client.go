package nodeclient

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"sort"
	"strings"
	"sync"
	"time"
)

var (
	ErrNoCompatible = errors.New("no compatible node")
	ErrCreateFailed = errors.New("sandbox create failed")
)

type CreateRequest struct {
	SandboxID       string            `json:"sandboxID"`
	TemplateID      string            `json:"templateID"`
	VCPU            int               `json:"vcpu"`
	MemMiB          int               `json:"memMiB"`
	EnvVars         map[string]string `json:"envVars"`
	AllowInternet   bool              `json:"allowInternet"`
	EnvdAccessToken string            `json:"envdAccessToken"`
}

type CreateResponse struct {
	SandboxID string `json:"sandboxID"`
	State     string `json:"state"`
	GuestURL  string `json:"guestURL"`
}

type capacity struct {
	CPUMillis       int  `json:"cpuMillis"`
	MemMiB          int  `json:"memMiB"`
	CommittedMemMiB int  `json:"committedMemMiB"`
	Running         int  `json:"running"`
	Ready           bool `json:"ready"`
}

type nodeSnap struct {
	url string
	cap capacity
}

type Client struct {
	HTTP *http.Client
}

func New() *Client {
	return &Client{HTTP: &http.Client{Timeout: 30 * time.Second}}
}

// Place polls capacity, picks the ready node with the lowest committedMemMiB,
// and retries the next node once if that node refuses with 503.
func (c *Client) Place(ctx context.Context, nodeURLs []string, req CreateRequest) (string, CreateResponse, error) {
	ready := c.readyNodes(ctx, nodeURLs)
	if len(ready) == 0 {
		return "", CreateResponse{}, ErrNoCompatible
	}
	sort.Slice(ready, func(i, j int) bool {
		if ready[i].cap.CommittedMemMiB != ready[j].cap.CommittedMemMiB {
			return ready[i].cap.CommittedMemMiB < ready[j].cap.CommittedMemMiB
		}
		return ready[i].url < ready[j].url
	})
	nodeURL, resp, status, err := c.create(ctx, ready[0].url, req)
	if err == nil && status == http.StatusOK {
		return nodeURL, resp, nil
	}
	if status == http.StatusServiceUnavailable && len(ready) > 1 {
		nodeURL, resp, status, err = c.create(ctx, ready[1].url, req)
		if err == nil && status == http.StatusOK {
			return nodeURL, resp, nil
		}
	}
	if status == http.StatusServiceUnavailable || err != nil {
		return "", CreateResponse{}, ErrCreateFailed
	}
	return "", CreateResponse{}, ErrCreateFailed
}

func (c *Client) Delete(ctx context.Context, nodeURL, sandboxID string) error {
	u := strings.TrimRight(nodeURL, "/") + "/v1/sandboxes/" + url.PathEscape(sandboxID)
	req, err := http.NewRequestWithContext(ctx, http.MethodDelete, u, nil)
	if err != nil {
		return err
	}
	res, err := c.http().Do(req)
	if err != nil {
		return err
	}
	defer res.Body.Close()
	io.Copy(io.Discard, res.Body)
	if res.StatusCode == http.StatusNoContent || res.StatusCode == http.StatusNotFound {
		return nil
	}
	return fmt.Errorf("node delete status %d", res.StatusCode)
}

func (c *Client) readyNodes(ctx context.Context, nodeURLs []string) []nodeSnap {
	var mu sync.Mutex
	var wg sync.WaitGroup
	var ready []nodeSnap
	for _, raw := range nodeURLs {
		raw = strings.TrimSpace(raw)
		if raw == "" {
			continue
		}
		wg.Add(1)
		go func(nodeURL string) {
			defer wg.Done()
			cap, ok := c.capacity(ctx, nodeURL)
			if !ok || !cap.Ready {
				return
			}
			mu.Lock()
			ready = append(ready, nodeSnap{url: nodeURL, cap: cap})
			mu.Unlock()
		}(raw)
	}
	wg.Wait()
	return ready
}

func (c *Client) capacity(ctx context.Context, nodeURL string) (capacity, bool) {
	cctx, cancel := context.WithTimeout(ctx, 3*time.Second)
	defer cancel()
	req, err := http.NewRequestWithContext(cctx, http.MethodGet, strings.TrimRight(nodeURL, "/")+"/v1/capacity", nil)
	if err != nil {
		return capacity{}, false
	}
	res, err := c.http().Do(req)
	if err != nil {
		return capacity{}, false
	}
	defer res.Body.Close()
	if res.StatusCode != http.StatusOK {
		io.Copy(io.Discard, res.Body)
		return capacity{}, false
	}
	var cap capacity
	if err := json.NewDecoder(res.Body).Decode(&cap); err != nil {
		return capacity{}, false
	}
	return cap, true
}

func (c *Client) create(ctx context.Context, nodeURL string, body CreateRequest) (string, CreateResponse, int, error) {
	if body.EnvVars == nil {
		body.EnvVars = map[string]string{}
	}
	buf, err := json.Marshal(body)
	if err != nil {
		return "", CreateResponse{}, 0, err
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, strings.TrimRight(nodeURL, "/")+"/v1/sandboxes", bytes.NewReader(buf))
	if err != nil {
		return "", CreateResponse{}, 0, err
	}
	req.Header.Set("Content-Type", "application/json")
	res, err := c.http().Do(req)
	if err != nil {
		return "", CreateResponse{}, 0, err
	}
	defer res.Body.Close()
	raw, _ := io.ReadAll(res.Body)
	if res.StatusCode != http.StatusOK {
		return "", CreateResponse{}, res.StatusCode, fmt.Errorf("node create status %d", res.StatusCode)
	}
	var out CreateResponse
	if err := json.Unmarshal(raw, &out); err != nil {
		return "", CreateResponse{}, res.StatusCode, err
	}
	return nodeURL, out, res.StatusCode, nil
}

func (c *Client) http() *http.Client {
	if c.HTTP != nil {
		return c.HTTP
	}
	return http.DefaultClient
}
