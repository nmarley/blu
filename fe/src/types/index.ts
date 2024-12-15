export interface FileEntry {
  contentHash: string;
  size: number;
  status: 'ready' | 'uploaded' | 'uploading' | 'unencrypted';
  tags: string[];
  paths: string[];
  chunks?: {
    count: number;
    sizes: number[];
  };
}

export interface KeyStatus {
  loaded: boolean;
  locked: boolean;
}

export interface GlobalStatus {
  totalFiles: number;
  encryptedCount: number;
  uploadedCount: number;
  pendingCount: number;
  unencryptedCount: number;
  storageUsed: string;
}