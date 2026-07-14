# State capture functionality for Python
# This code is injected into user scripts for experimental state capture

import os

def upmd_capture_state():
    """Capture environment variables and current working directory."""
    upmd_write_state()

def upmd_state_escape(value):
    return (
        value
        .replace('\\', '\\\\')
        .replace('"', '\\"')
        .replace('\n', '\\n')
        .replace('\r', '\\r')
        .replace('\t', '\\t')
    )

def upmd_write_state():
    """Write environment variables and cwd to FIFO as upmd state v1."""
    state_fifo = os.environ.get('UPMD_STATE_FIFO', '/dev/null')
    try:
        with open(state_fifo, 'w') as f:
            f.write('version 1\n')
            f.write(f'cwd "{upmd_state_escape(os.getcwd())}"\n')
            for key, value in os.environ.items():
                f.write(f'env "{upmd_state_escape(key)}" "{upmd_state_escape(value)}"\n')
    except Exception:
        pass  # Silently ignore state capture errors
