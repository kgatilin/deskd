/**
 * MessageComposer — text input for sending messages to agents.
 *
 * Enter sends, Esc cancels.
 */

import { useState } from "react";
import { Box, Text, useInput } from "ink";
import { colors } from "../theme.js";

interface MessageComposerProps {
  target: string;
  onSend: (text: string) => void;
  onCancel: () => void;
}

export function MessageComposer({
  target,
  onSend,
  onCancel,
}: MessageComposerProps) {
  const [text, setText] = useState("");

  useInput((input, key) => {
    if (key.escape) {
      onCancel();
      return;
    }
    if (key.return) {
      if (text.trim()) {
        onSend(text.trim());
      }
      return;
    }
    if (key.backspace || key.delete) {
      setText((prev) => prev.slice(0, -1));
      return;
    }
    if (input && !key.ctrl && !key.meta) {
      setText((prev) => prev + input);
    }
  });

  return (
    <Box
      flexDirection="column"
      borderStyle="single"
      borderColor={colors.primary}
      paddingX={2}
      paddingY={1}
    >
      <Text bold color={colors.primary}>
        Send message to <Text color={colors.secondary}>{target}</Text>
      </Text>
      <Box>
        <Text color={colors.accent}>&gt; </Text>
        <Text color={colors.text}>{text}</Text>
        <Text color={colors.accent}>_</Text>
      </Box>
      <Text color={colors.textDim}>
        Enter: send | Esc: cancel
      </Text>
    </Box>
  );
}
