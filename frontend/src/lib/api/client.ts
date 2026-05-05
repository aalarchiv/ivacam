// Transport-agnostic client interface. The HTTP implementation in `http.ts`
// talks to the Stage-1 FastAPI bridge or the future Rust axum server.
// Tauri and WASM implementations land in d0d.7 and d0d.8.

import type {
  GenerateRequest,
  GenerateResponse,
  ImportResponse,
  VersionResponse,
} from './types';

export interface WiacClient {
  health(): Promise<boolean>;
  version(): Promise<VersionResponse>;
  importFile(file: File, format?: string): Promise<ImportResponse>;
  generate(request: GenerateRequest): Promise<GenerateResponse>;
}
