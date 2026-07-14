func upmdCaptureState() {
	upmdWriteState()
}

func upmdStateEscape(value string) string {
	return strings.NewReplacer(
		`\`, `\\`,
		`"`, `\"`,
		"\n", `\n`,
		"\r", `\r`,
		"\t", `\t`,
	).Replace(value)
}

func upmdWriteState() {
	stateFifo := os.Getenv("UPMD_STATE_FIFO")
	if stateFifo == "" {
		return
	}

	file, err := os.OpenFile(stateFifo, os.O_WRONLY|os.O_CREATE|os.O_TRUNC, 0644)
	if err != nil {
		return
	}
	defer file.Close()

	cwd, err := os.Getwd()
	if err != nil {
		cwd = ""
	}

	fmt.Fprintln(file, "version 1")
	fmt.Fprintf(file, "cwd \"%s\"\n", upmdStateEscape(cwd))
	for _, env := range os.Environ() {
		parts := strings.SplitN(env, "=", 2)
		if len(parts) == 2 {
			fmt.Fprintf(file, "env \"%s\" \"%s\"\n", upmdStateEscape(parts[0]), upmdStateEscape(parts[1]))
		}
	}
}
