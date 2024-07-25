package main

import (
	"context"
	"log"
	"os"
	"os/signal"
	"syscall"

	"github.com/spf13/cobra"
)

var rootCmd = &cobra.Command{
	Use:    "castle",
	Hidden: true,
	RunE: func(cmd *cobra.Command, args []string) error {
		return cmd.Help()
	},
}

var httpCmd = &cobra.Command{
	Use:  "http",
	Args: cobra.ExactArgs(1),
	RunE: func(cmd *cobra.Command, args []string) error {
		return nil
	},
}

var tcpCmd = &cobra.Command{
	Use:  "tcp",
	Args: cobra.ExactArgs(1),
	RunE: func(cmd *cobra.Command, args []string) error {
		return nil
	},
}

var udpCmd = &cobra.Command{
	Use:  "udp",
	Args: cobra.ExactArgs(1),
	RunE: func(cmd *cobra.Command, args []string) error {
		return nil
	},
}

func init() {
	rootCmd.PersistentFlags().String("server-addr", "127.0.0.1:6610", "")

	httpCmd.Flags().String("domain", "localhost", "Domain")
	httpCmd.Flags().String("subdomain", "", "")
	httpCmd.Flags().Bool("random-subdomain", false, "Random subdomain")
	httpCmd.Flags().Int16("remote-port", 0, "Remote port")
	httpCmd.Flags().String("local-addr", "127.0.0.1", "Domain")

	tcpCmd.Flags().Int16("remote-port", 0, "Remote port")
	tcpCmd.Flags().String("local-addr", "127.0.0.1", "Domain")

	udpCmd.Flags().Int16("remote-port", 0, "Remote port")
	udpCmd.Flags().String("local-addr", "127.0.0.1", "Domain")

	rootCmd.AddCommand(tcpCmd, udpCmd, httpCmd)
}

func main() {
	ctx := context.Background()
	ctx, cancel := signal.NotifyContext(ctx, os.Interrupt, syscall.SIGTERM)
	defer cancel()

	if _, err := rootCmd.ExecuteContextC(ctx); err != nil {
		log.Fatal(err)
	}
}
