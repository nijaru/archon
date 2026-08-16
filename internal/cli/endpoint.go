package cli

import (
	"github.com/rs/zerolog/log"
	"github.com/spf13/cobra"
)

var endpointCmd = &cobra.Command{
	Use:   "endpoint",
	Short: "Manage inference endpoints and deployments",
}

var endpointCreateCmd = &cobra.Command{
	Use:   "create [name] [model_version_id]",
	Short: "Create an endpoint routing to a specific model version",
	Args:  cobra.ExactArgs(2),
	Run: func(cmd *cobra.Command, args []string) {
		name := args[0]
		versionID := args[1]
		log.Info().Fields(map[string]interface{}{
			"name":       name,
			"version_id": versionID,
		}).Msg("Creating inference endpoint...")
	},
}

func init() {
	endpointCmd.AddCommand(endpointCreateCmd)
	RootCmd.AddCommand(endpointCmd)
}
