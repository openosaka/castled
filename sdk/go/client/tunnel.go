package client

import (
	"github.com/openosaka/castled/sdk/go/proto"
)

type Tunnel struct {
	proto.Tunnel

	Name      string
	LocalAddr string
}

type tcpOptions struct {
	port int16
}

type tcpOption func(*tcpOptions)

func WithTCPPort(port int16) tcpOption {
	return func(opts *tcpOptions) {
		opts.port = port
	}
}

// NewTCPTunnel creates a new TCP tunnel.
//
// Without any option, the default behavior is to create a tunnel with a random port.
func NewTCPTunnel(name, localAddr string, options ...tcpOption) *Tunnel {
	opts := &tcpOptions{}
	for _, option := range options {
		option(opts)
	}

	return &Tunnel{
		Tunnel: proto.Tunnel{
			Name: name,
			Type: proto.Tunnel_UDP,
			Config: &proto.Tunnel_Udp{
				Udp: &proto.UDPConfig{
					RemotePort: int32(opts.port),
				},
			},
		},
		LocalAddr: localAddr,
	}
}

type udpOptions struct {
	port int16
}

type udpOption func(*udpOptions)

func WithUdpPort(port int16) udpOption {
	return func(opts *udpOptions) {
		opts.port = port
	}
}

// NewUDPTunnel creates a new UDP tunnel.
//
// Without any option, the default behavior is to create a tunnel with a random port.
func NewUDPTunnel(name, localAddr string, options ...udpOption) *Tunnel {
	opts := &udpOptions{}
	for _, option := range options {
		option(opts)
	}

	return &Tunnel{
		Tunnel: proto.Tunnel{
			Name: name,
			Type: proto.Tunnel_UDP,
			Config: &proto.Tunnel_Udp{
				Udp: &proto.UDPConfig{
					RemotePort: int32(opts.port),
				},
			},
		},
		LocalAddr: localAddr,
	}
}

type httpOptions struct {
	domain          string
	subDomain       string
	randomSubdomain bool
	port            int16
}

func WithHTTPPort(port int16) httpOption {
	return func(opts *httpOptions) {
		opts.port = port
	}
}

func WithHTTPDomain(domain string) httpOption {
	return func(opts *httpOptions) {
		opts.domain = domain
	}
}

func WithHTTPSubDomain(subDomain string) httpOption {
	return func(opts *httpOptions) {
		opts.subDomain = subDomain
	}
}

func WithHTTPRandomSubdomain(randomSubdomain bool) httpOption {
	return func(opts *httpOptions) {
		opts.randomSubdomain = randomSubdomain
	}
}

type httpOption func(*httpOptions)

// NewHTTPTunnel creates a new HTTP tunnel.
//
// Without any option, the default behavior is to create a tunnel with a random port.
func NewHTTPTunnel(name, localAddr string, options ...httpOption) *Tunnel {
	opts := &httpOptions{}
	for _, option := range options {
		option(opts)
	}

	return &Tunnel{
		Tunnel: proto.Tunnel{
			Name: name,
			Type: proto.Tunnel_UDP,
			Config: &proto.Tunnel_Udp{
				Udp: &proto.UDPConfig{
					RemotePort: int32(opts.port),
				},
			},
		},
		LocalAddr: localAddr,
	}
}
