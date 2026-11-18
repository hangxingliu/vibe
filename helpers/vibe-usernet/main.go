// This was entirely vibe-coded. It works, but I'm sure could be better. PRs welcome.

package main

import (
	"bufio"
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"net"
	"net/url"
	"os"
	"os/signal"
	"strings"
	"syscall"
	"time"

	"github.com/containers/gvisor-tap-vsock/pkg/services/dns"
	"github.com/containers/gvisor-tap-vsock/pkg/services/noproxy"
	"github.com/containers/gvisor-tap-vsock/pkg/types"
	"github.com/containers/gvisor-tap-vsock/pkg/virtualnetwork"
)

const (
	defaultFD               = 3
	defaultParentLivenessFD = 4

	subnet       = "192.168.5.0/24"
	gatewayIP    = "192.168.5.2"
	guestIP      = "192.168.5.15"
	gatewayMAC   = "5a:94:ef:e4:0c:dd"
	defaultMTU   = 1500
	readyMessage = "ready"

	// dnsUpstreamsPrefix starts the line, printed before readyMessage, that
	// describes the gateway's DNS upstreams.
	dnsUpstreamsPrefix = "dns-upstreams"
)

type stringSlice []string

func (s *stringSlice) String() string { return strings.Join(*s, ", ") }
func (s *stringSlice) Set(v string) error {
	*s = append(*s, v)
	return nil
}

func main() {
	if err := run(); err != nil {
		fmt.Fprintf(os.Stderr, "vibe-usernet: %v\n", err)
		os.Exit(1)
	}
}

// config is the parsed command line.
type config struct {
	fd               int
	parentLivenessFD int
	guestMAC         string
	forwards         map[string]string
	proxy            string
	proxyBypass      *string
	proxyUDP         bool
	proxyDNS         bool
	strictProxy      bool
	dnsUpstreams     []string
}

func parseConfig(args []string) (config, error) {
	fs := flag.NewFlagSet("vibe-usernet", flag.ContinueOnError)
	fd := fs.Int("fd", defaultFD, "connected VZ datagram file descriptor")
	parentLivenessFD := fs.Int("parent-liveness-fd", defaultParentLivenessFD, "parent liveness file descriptor")
	guestMAC := fs.String("mac", "", "guest MAC address")
	var publish stringSlice
	fs.Var(&publish, "p", "publish port: [udp:][host_addr:]host_port:guest_port")
	proxy := fs.String("proxy", "", "Proxy URL for outbound guest traffic (http://, https://, or socks5://). Destinations in the bypass list and the gateway's NAT target are reached directly.")
	proxyBypass := fs.String("proxy-bypass", "", "Comma-separated bypass list replacing the built-in one: *, CIDR, CIDR:port (IPv4), [CIDR]:port. An empty value bypasses nothing. Requires --proxy.")
	proxyUDP := fs.Bool("proxy-udp", false, "Send UDP through the proxy (socks5:// only). Default: true for socks5://, false otherwise. Requires --proxy.")
	proxyDNS := fs.Bool("proxy-dns", false, "Send the gateway's DNS queries through the proxy as DNS-over-TCP (socks5:// only). Default: true for socks5://, false otherwise. Requires --proxy.")
	strictProxy := fs.Bool("strict-proxy", false, "Drop UDP, ICMP, and DNS that the proxy cannot carry, except for destinations in the bypass list. Requires --proxy.")
	var dnsUpstreams stringSlice
	fs.Var(&dnsUpstreams, "dns", "Upstream DNS server address (e.g. 8.8.8.8 or 1.1.1.1:53); repeatable. Replaces the host's nameservers for gateway DNS. With --proxy-dns, upstreams outside the bypass list are queried through the proxy.")
	if err := fs.Parse(args); err != nil {
		return config{}, err
	}

	if fs.NArg() != 0 {
		return config{}, fmt.Errorf("unexpected arguments: %s", strings.Join(fs.Args(), " "))
	}
	if _, err := net.ParseMAC(*guestMAC); err != nil {
		return config{}, fmt.Errorf("invalid --mac: %w", err)
	}

	set := map[string]bool{}
	fs.Visit(func(f *flag.Flag) { set[f.Name] = true })

	cfg := config{
		fd:               *fd,
		parentLivenessFD: *parentLivenessFD,
		guestMAC:         *guestMAC,
		forwards:         make(map[string]string),
		proxy:            *proxy,
		dnsUpstreams:     []string(dnsUpstreams),
	}

	if *proxy == "" {
		for _, name := range []string{"proxy-bypass", "proxy-udp", "proxy-dns", "strict-proxy"} {
			if set[name] {
				return config{}, fmt.Errorf("--%s requires --proxy", name)
			}
		}
	} else {
		u, err := url.Parse(*proxy)
		if err != nil || u.Host == "" {
			return config{}, fmt.Errorf("invalid --proxy URL")
		}
		scheme := strings.ToLower(u.Scheme)
		socks := scheme == "socks5"
		if !socks && scheme != "http" && scheme != "https" {
			return config{}, fmt.Errorf("unsupported --proxy scheme %q; use http://, https://, or socks5://", u.Scheme)
		}
		cfg.proxyUDP = socks
		if set["proxy-udp"] {
			cfg.proxyUDP = *proxyUDP
		}
		cfg.proxyDNS = socks
		if set["proxy-dns"] {
			cfg.proxyDNS = *proxyDNS
		}
		cfg.strictProxy = *strictProxy
		if cfg.proxyUDP && !socks {
			return config{}, fmt.Errorf("--proxy-udp is only supported with socks5:// proxies")
		}
		if cfg.proxyDNS && !socks {
			return config{}, fmt.Errorf("--proxy-dns is only supported with socks5:// proxies")
		}
		if set["proxy-bypass"] {
			if _, err := noproxy.Parse(*proxyBypass); err != nil {
				return config{}, fmt.Errorf("invalid --proxy-bypass: %w", err)
			}
			bypass := *proxyBypass
			cfg.proxyBypass = &bypass
		}
	}

	for _, spec := range publish {
		local, remote, err := parsePublish(spec)
		if err != nil {
			return config{}, err
		}
		if _, exists := cfg.forwards[local]; exists {
			return config{}, fmt.Errorf("duplicate publish for %s", local)
		}
		cfg.forwards[local] = remote
	}
	return cfg, nil
}

func (cfg config) networkConfiguration() *types.Configuration {
	return &types.Configuration{
		Debug:             false,
		MTU:               defaultMTU,
		Subnet:            subnet,
		GatewayIP:         gatewayIP,
		GatewayMacAddress: gatewayMAC,
		DHCPStaticLeases: map[string]string{
			gatewayIP: gatewayMAC,
			guestIP:   cfg.guestMAC,
		},
		Forwards:          cfg.forwards,
		DNS:               []types.Zone{},
		DNSSearchDomains:  searchDomains(),
		NAT:               map[string]string{gatewayIP: "127.0.0.1"},
		GatewayVirtualIPs: []string{gatewayIP},
		Proxy:             cfg.proxy,
		ProxyBypass:       cfg.proxyBypass,
		ProxyUDP:          cfg.proxyUDP,
		NoProxyDNS:        !cfg.proxyDNS,
		StrictProxy:       cfg.strictProxy,
		DNSUpstreams:      cfg.dnsUpstreams,
	}
}

// dnsUpstreamsLine describes the gateway's DNS upstreams for the parent
// process, as "dns-upstreams <mode> <description>".
func dnsUpstreamsLine(configuration *types.Configuration) (string, error) {
	_, upstream, err := virtualnetwork.ProxySettings(configuration)
	if err != nil {
		return "", err
	}
	plan, err := dns.PlanUpstreams(upstream)
	if err != nil {
		return "", err
	}
	return fmt.Sprintf("%s %s %s", dnsUpstreamsPrefix, plan.Mode, plan.Describe()), nil
}

func run() error {
	cfg, err := parseConfig(os.Args[1:])
	if err != nil {
		return err
	}

	ctx, stopSignals := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stopSignals()
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()

	if cfg.parentLivenessFD >= 0 {
		watchParentLiveness(ctx, cancel, cfg.parentLivenessFD)
	}

	conn, err := fileConn(cfg.fd, "vibe-usernet")
	if err != nil {
		return err
	}
	defer conn.Close()

	configuration := cfg.networkConfiguration()
	vn, err := virtualnetwork.New(configuration)
	if err != nil {
		return err
	}
	dnsLine, err := dnsUpstreamsLine(configuration)
	if err != nil {
		return err
	}

	errCh := make(chan error, 1)
	go func() {
		errCh <- vn.AcceptVfkit(ctx, &udpFileConn{Conn: conn})
	}()

	fmt.Fprintln(os.Stdout, dnsLine)
	fmt.Fprintln(os.Stdout, readyMessage)

	select {
	case <-ctx.Done():
		return nil
	case err := <-errCh:
		if err == nil || errors.Is(err, net.ErrClosed) || ctx.Err() != nil {
			return nil
		}
		return err
	}
}

// parsePublish parses a publish spec of the form [udp:][host_addr:]host_port:guest_port
// and returns the local (host) listen address and the remote (guest) target address.
// If the spec starts with "udp:", the local address will be prefixed with "udp:".
func parsePublish(spec string) (local, remote string, err error) {
	isUDP := false
	if strings.HasPrefix(spec, "udp:") {
		isUDP = true
		spec = strings.TrimPrefix(spec, "udp:")
	}

	parts := strings.Split(spec, ":")
	switch len(parts) {
	case 2:
		// host_port:guest_port
		local = "0.0.0.0:" + parts[0]
		remote = guestIP + ":" + parts[1]
	case 3:
		// host_addr:host_port:guest_port
		local = parts[0] + ":" + parts[1]
		remote = guestIP + ":" + parts[2]
	default:
		return "", "", fmt.Errorf("invalid publish spec %q: expected [udp:][host_addr:]host_port:guest_port", spec)
	}

	// Validate ports
	if _, _, err := net.SplitHostPort(local); err != nil {
		return "", "", fmt.Errorf("invalid publish spec %q: %w", spec, err)
	}
	if _, _, err := net.SplitHostPort(remote); err != nil {
		return "", "", fmt.Errorf("invalid publish spec %q: %w", spec, err)
	}

	if isUDP {
		local = "udp:" + local
	}

	return local, remote, nil
}

func fileConn(fd int, name string) (net.Conn, error) {
	file := os.NewFile(uintptr(fd), name)
	if file == nil {
		return nil, fmt.Errorf("invalid fd %d", fd)
	}
	defer file.Close()

	conn, err := net.FileConn(file)
	if err != nil {
		return nil, fmt.Errorf("open fd %d: %w", fd, err)
	}
	return conn, nil
}

func watchParentLiveness(ctx context.Context, cancel context.CancelFunc, fd int) {
	file := os.NewFile(uintptr(fd), "parent-liveness")
	if file == nil {
		cancel()
		return
	}

	go func() {
		defer cancel()
		defer file.Close()
		_, _ = io.Copy(io.Discard, file)
	}()

	go func() {
		<-ctx.Done()
		_ = file.Close()
	}()
}

func searchDomains() []string {
	file, err := os.Open("/etc/resolv.conf")
	if err != nil {
		return nil
	}
	defer file.Close()

	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		if after, ok := strings.CutPrefix(scanner.Text(), "search "); ok {
			return strings.Fields(after)
		}
	}
	return nil
}

type udpFileConn struct {
	net.Conn
}

func (conn *udpFileConn) Read(b []byte) (int, error) {
	if err := conn.SetReadDeadline(time.Time{}); err != nil {
		if errors.Is(err, net.ErrClosed) {
			return 0, errors.New("udpFileConn closed")
		}
	}
	return conn.Conn.Read(b)
}
