package cli

import (
	"github.com/rs/zerolog/log"
	"github.com/spf13/cobra"
)

var modelCmd = &cobra.Command{
	Use:   "model",
	Short: "Manage models and versions in the model registry",
}

var modelRegisterCmd = &cobra.Command{
	Use:   "register [name] [source]",
	Short: "Register a model from S3 or Hugging Face Hub",
	Args:  cobra.ExactArgs(2),
	Run: func(cmd *cobra.Command, args []string) {
		name := args[0]
		source := args[1]
		log.Info().Fields(map[string]interface{}{
			"name":   name,
			"source": source,
		}).Msg("Registering model...")
	},
}

func init() {
	modelCmd.AddCommand(modelRegisterCmd)
	RootCmd.AddCommand(modelCmd)
}
