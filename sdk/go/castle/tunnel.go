package castle

import (
	"github.com/openosaka/castled/sdk/go/proto"
)

type Tunnel struct {
	proto.Tunnel

	Name      string
	LocalAddr string
}

type tcpOptions struct {
	port uint16
}

type TCPOption func(*tcpOptions)

func WithTCPPort(port uint16) TCPOption {
	return func(opts *tcpOptions) {
		opts.port = port
	}
}

// NewTCPTunnel creates a new TCP tunnel.
//
// Without any option, the default behavior is to create a tunnel with a random port.
func NewTCPTunnel(name, localAddr string, options ...TCPOption) *Tunnel {
	opts := &tcpOptions{}
	for _, option := range options {
		option(opts)
	}

	return &Tunnel{
		Tunnel: proto.Tunnel{
			Name: name,
			Config: &proto.Tunnel_Tcp{
				Tcp: &proto.TCPConfig{
					RemotePort: int32(opts.port),
				},
			},
		},
		LocalAddr: localAddr,
	}
}

type udpOptions struct {
	port uint16
}

type UDPOption func(*udpOptions)

func WithUdpPort(port uint16) UDPOption {
	return func(opts *udpOptions) {
		opts.port = port
	}
}

// NewUDPTunnel creates a new UDP tunnel.
//
// Without any option, the default behavior is to create a tunnel with a random port.
func NewUDPTunnel(name, localAddr string, options ...UDPOption) *Tunnel {
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

func WithHTTPPort(port uint16) HTTPOption {
	return func(opts *httpOptions) {
		opts.pbFn = func() *proto.HTTPConfig {
			return &proto.HTTPConfig{
				RemotePort: int32(port),
			}
		}
	}
}

func WithHTTPDomain(domain string) HTTPOption {
	return func(opts *httpOptions) {
		opts.pbFn = func() *proto.HTTPConfig {
			return &proto.HTTPConfig{
				Domain: domain,
			}
		}
	}
}

func WithHTTPSubDomain(subDomain string) HTTPOption {
	return func(opts *httpOptions) {
		opts.pbFn = func() *proto.HTTPConfig {
			return &proto.HTTPConfig{
				Subdomain: subDomain,
			}
		}
	}
}

func WithHTTPRandomSubdomain() HTTPOption {
	return func(opts *httpOptions) {
		opts.pbFn = func() *proto.HTTPConfig {
			return &proto.HTTPConfig{
				RandomSubdomain: true,
			}
		}
	}
}

type HTTPOption func(*httpOptions)

// NewHTTPTunnel creates a new HTTP tunnel.
//
// Without any option, the default behavior is to create a tunnel with a random port.
func NewHTTPTunnel(name, localAddr string, options ...HTTPOption) *Tunnel {
	if len(options) > 1 {
		panic("only one option is allowed")
	}

	opts := &httpOptions{
		pbFn: func() *proto.HTTPConfig {
			return &proto.HTTPConfig{}
		},
	}
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
