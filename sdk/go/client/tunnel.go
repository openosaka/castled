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
	pbFn func() *proto.HTTPConfig
}

func WithHTTPPort(port int16) httpOption {
	return func(opts *httpOptions) {
		opts.pbFn = func() *proto.HTTPConfig {
			return &proto.HTTPConfig{
				RemotePort: int32(port),
			}
		}
	}
}

func WithHTTPDomain(domain string) httpOption {
	return func(opts *httpOptions) {
		opts.pbFn = func() *proto.HTTPConfig {
			return &proto.HTTPConfig{
				Domain: domain,
			}
		}
	}
}

func WithHTTPSubDomain(subDomain string) httpOption {
	return func(opts *httpOptions) {
		opts.pbFn = func() *proto.HTTPConfig {
			return &proto.HTTPConfig{
				Subdomain: subDomain,
			}
		}
	}
}

func WithHTTPRandomSubdomain(randomSubdomain bool) httpOption {
	return func(opts *httpOptions) {
		opts.pbFn = func() *proto.HTTPConfig {
			return &proto.HTTPConfig{
				RandomSubdomain: randomSubdomain,
			}
		}
	}
}

type httpOption func(*httpOptions)

// NewHTTPTunnel creates a new HTTP tunnel.
//
// Without any option, the default behavior is to create a tunnel with a random port.
func NewHTTPTunnel(name, localAddr string, options ...httpOption) *Tunnel {
	if len(options) > 1 {
		panic("only one option is allowed")
	}

	opts := &httpOptions{}
	for _, option := range options {
		option(opts)
	}

	return &Tunnel{
		Tunnel: proto.Tunnel{
			Name: name,
			Config: &proto.Tunnel_Http{
				Http: opts.pbFn(),
			},
		},
		LocalAddr: localAddr,
	}
}
