import type { StartupArtifact } from '@/lib/models/startup';

const SENSITIVE_OPTION = /(?:password|passwd|pass|token|secret|api[-_]?key|access[-_]?key|credential|auth)/iu;
const URI_CREDENTIALS = /^(\w+:\/\/)[^/@\s]+@/u;

export const StartupCommandUtils = {
  display(artifact: StartupArtifact, revealArguments: boolean): string {
    const target = artifact.target.path ?? artifact.target.executableName;
    const arguments_ = revealArguments ? artifact.target.arguments : redactArguments(artifact.target.arguments);
    return [target, ...arguments_].filter((value): value is string => Boolean(value)).join(' ');
  },
};

function redactArguments(arguments_: string[]): string[] {
  let redactNext = false;
  return arguments_.map(argument => {
    if (redactNext) {
      redactNext = false;
      return '••••';
    }
    const equals = argument.indexOf('=');
    if (equals > 0 && SENSITIVE_OPTION.test(argument.slice(0, equals))) {
      return `${argument.slice(0, equals + 1)}••••`;
    }
    if (argument.startsWith('-') && SENSITIVE_OPTION.test(argument)) {
      redactNext = true;
      return argument;
    }
    return argument.replace(URI_CREDENTIALS, '$1••••@');
  });
}
