// Transport-agnostic client interface. Implementations: HTTP (`http.ts`,
// talks to wiac-server), Tauri (`tauri.ts`, native invoke), and WASM
// (`wasm.ts`, runs the CAM pipeline in-browser).

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
  /**
   * Streaming variant: emits {phase, fraction, message} via the supplied
   * onProgress callback as the pipeline advances, returning the same
   * GenerateResponse the non-streaming `generate()` would. Falls back to
   * the non-streaming endpoint on transports that don't support streaming
   * (Tauri/WASM implementations land in d0d.7/.8).
   */
  generateStream?(
    request: GenerateRequest,
    onProgress: (e: ProgressEvent) => void,
  ): Promise<GenerateResponse>;
  defaults(): Promise<DefaultsResponse>;
}

export interface ProgressEvent {
  phase: string;
  fraction: number;
  message: string;
}

export interface DefaultsResponse {
  setup: Record<string, unknown>;
  schema: JsonSchema;
  definitions: Record<string, JsonSchema>;
}

export interface JsonSchema {
  type?: 'object' | 'string' | 'number' | 'integer' | 'boolean' | 'array' | 'null';
  description?: string;
  properties?: Record<string, JsonSchema>;
  required?: string[];
  enum?: string[];
  $ref?: string;
  format?: string;
  minimum?: number;
  maximum?: number;
  default?: unknown;
  items?: JsonSchema;
}
