package main

import (
	"fmt"
	"net"
	"os"

	"github.com/spf13/pflag"
)

var (
	port int
	host string
)

func main() {
	pflag.IntVar(&port, "port", 12345, "")
	pflag.StringVar(&host, "host", "0.0.0.0", "")
	pflag.Parse()

	// Listen for incoming UDP packets on port 12345
	addr := net.UDPAddr{
		Port: port,
		IP:   net.ParseIP(host),
	}
	conn, err := net.ListenUDP("udp", &addr)
	if err != nil {
		fmt.Println("Error listening:", err)
		os.Exit(1)
	}
	defer conn.Close()

	buffer := make([]byte, 1024)

	for {
		n, clientAddr, err := conn.ReadFromUDP(buffer)
		if err != nil {
			fmt.Println("Error reading from UDP:", err)
			continue
		}

		message := string(buffer[:n])
		fmt.Printf("Received message from %s: %s", clientAddr, message)
	}
}
