// Public sandbox-runtime API. Importing this module also defines the
// <sandbox-iframe> custom element (side effect of importing sandboxed-iframe).

export {
  SandboxIframe,
  SANDBOX_EXECUTE_TIMEOUT_MS,
} from "./sandboxed-iframe";
export type {
  SandboxFile,
  SandboxResult,
  SandboxUrlProvider,
  PrepareHtmlOptions,
} from "./sandboxed-iframe";

export {
  RUNTIME_MESSAGE_ROUTER,
  RuntimeMessageRouter,
} from "./runtime-message-router";
export type { MessageConsumer } from "./runtime-message-router";

export { RuntimeMessageBridge } from "./runtime-message-bridge";
export type { MessageType, RuntimeMessageBridgeOptions } from "./runtime-message-bridge";

export type { SandboxRuntimeProvider } from "./providers/provider";

export { ConsoleRuntimeProvider } from "./providers/console-provider";
export type { ConsoleLog } from "./providers/console-provider";

export { ArtifactsRuntimeProvider } from "./providers/artifacts-provider";
export type { ArtifactsHost, ArtifactsAgentHost } from "./providers/artifacts-provider";

export { AttachmentsRuntimeProvider } from "./providers/attachments-provider";

export {
  FileDownloadRuntimeProvider,
  encodeFileContent,
  encodeDownloadableFile,
  BASE64_CHUNK_SIZE,
} from "./providers/file-download-provider";
export type {
  DownloadableFile,
  EncodedDownloadableFile,
} from "./providers/file-download-provider";
