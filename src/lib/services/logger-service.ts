import { isTauri } from '@tauri-apps/api/core';
import { error as writeNativeError, info as writeNativeInfo, warn as writeNativeWarn } from '@tauri-apps/plugin-log';

type LogContext = Readonly<Record<string, unknown>>;
type LogLevel = 'info' | 'warn' | 'error';
type NativeLogWriter = (message: string) => Promise<void>;

/**
 * Central frontend logging boundary. Production code emits stable domains,
 * events, and context without depending on the current log destination.
 */
export class LoggerService {
  static info(domain: string, event: string, context?: LogContext): void {
    LoggerService.write('info', domain, event, context);
  }

  static warn(domain: string, event: string, context?: LogContext): void {
    LoggerService.write('warn', domain, event, context);
  }

  static error(domain: string, event: string, context?: LogContext): void {
    LoggerService.write('error', domain, event, context);
  }

  private static write(level: LogLevel, domain: string, event: string, context?: LogContext): void {
    const message = `[${domain}] ${event}`;
    const args: [string] | [string, LogContext] = context ? [message, context] : [message];

    LoggerService.writeNative(level, message);

    if (level === 'error') {
      console.error(...args);
      return;
    }
    if (level === 'warn') {
      console.warn(...args);
      return;
    }
    console.info(...args);
  }

  private static writeNative(level: LogLevel, message: string): void {
    // Browser previews and unit tests intentionally retain console-only
    // logging. Native logs persist only stable domain/event identifiers; the
    // richer console context may contain private filesystem paths.
    if (!isTauri()) return;

    const writers: Readonly<Record<LogLevel, NativeLogWriter>> = {
      error: writeNativeError,
      info: writeNativeInfo,
      warn: writeNativeWarn,
    };
    void writers[level](message).catch(error => {
      console.warn('[frontend-logging] native_log_failed', {
        level,
        error: error instanceof Error ? error.name : typeof error,
      });
    });
  }
}
