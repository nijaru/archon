package cli

import (
	"github.com/rs/zerolog/log"
	"github.com/spf13/cobra"
)

var nodeCmd = &cobra.Command{
	Use:   "node",
	Short: "Manage physical GPU nodes in the fleet",
}

var nodeJoinCmd = &cobra.Command{
	Use:   "join",
	Short: "Start the direct node agent and join the control plane",
	Run: func(cmd *cobra.Command, args []string) {
		log.Info().Msg("Starting direct node agent daemon...")
		// The actual direct agent daemon logic will be wired here in Sprint 1
	},
}

func init() {
	nodeCmd.AddCommand(nodeJoinCmd)
	RootCmd.AddCommand(nodeCmd)
}
