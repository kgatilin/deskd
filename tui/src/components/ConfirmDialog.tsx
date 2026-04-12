/**
 * ConfirmDialog — y/n confirmation overlay.
 *
 * Shows a message and waits for y (confirm) or n/Esc (cancel).
 */

import { Box, Text, useInput } from "ink";
import { colors } from "../theme.js";

interface ConfirmDialogProps {
  message: string;
  onConfirm: () => void;
  onCancel: () => void;
}

export function ConfirmDialog({
  message,
  onConfirm,
  onCancel,
}: ConfirmDialogProps) {
  useInput((input, key) => {
    if (input === "y" || input === "Y") {
      onConfirm();
      return;
    }
    if (input === "n" || input === "N" || key.escape) {
      onCancel();
      return;
    }
  });

  return (
    <Box
      flexDirection="column"
      borderStyle="double"
      borderColor={colors.warning}
      paddingX={2}
      paddingY={1}
    >
      <Text bold color={colors.warning}>
        Confirm
      </Text>
      <Text color={colors.text}>{message}</Text>
      <Text color={colors.textDim}>
        Press <Text bold color={colors.success}>y</Text> to confirm,{" "}
        <Text bold color={colors.error}>n</Text> or{" "}
        <Text bold>Esc</Text> to cancel
      </Text>
    </Box>
  );
}
