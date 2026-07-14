// State capture functionality for TypeScript
// This code is injected into user scripts for experimental state capture

import * as fs from 'fs';
import * as process from 'process';

function upmdCaptureState(): void {
    upmdWriteState();
}

function upmdStateEscape(value: string): string {
    return value
        .replace(/\\/g, '\\\\')
        .replace(/"/g, '\\"')
        .replace(/\n/g, '\\n')
        .replace(/\r/g, '\\r')
        .replace(/\t/g, '\\t');
}

function upmdWriteState(): void {
    const stateFifo = process.env.UPMD_STATE_FIFO;
    if (!stateFifo) return;

    try {
        const lines = [
            'version 1',
            `cwd "${upmdStateEscape(process.cwd())}"`,
        ];
        for (const [key, value] of Object.entries(process.env)) {
            lines.push(`env "${upmdStateEscape(key)}" "${upmdStateEscape(value || '')}"`);
        }
        fs.writeFileSync(stateFifo, `${lines.join('\n')}\n`);
    } catch (error) {
        // Silently ignore errors
    }
}
