import { useState } from 'react';
import { Header } from '@/components/layout/Header';
import { Sidebar } from '@/components/navigation/Sidebar';
import { FileList } from '@/components/file-browser/FileList';
import { KeyManager } from '@/components/key-management/KeyManager';
import { FileEntry, KeyStatus, GlobalStatus } from '@/types';
import { ThemeProvider } from '@/components/theme-provider';
import { Toaster } from '@/components/ui/toaster';

// Mock data for demonstration
const mockFiles: FileEntry[] = [
  {
    contentHash: "1ce404e1234567890abcdef",
    size: 14500000,
    status: "ready",
    tags: [],
    paths: ["samsung", "/ENG/docs/spec.pdf"],
  },
  {
    contentHash: "589a38abcdef1234567890",
    size: 11500000,
    status: "uploading",
    tags: [],
    paths: ["rm0367"],
  },
  {
    contentHash: "b3f9c3567890abcdef1234",
    size: 1400000,
    status: "unencrypted",
    tags: ["sensor", "bosch", "temp"],
    paths: ["bst-bme280"],
  },
];

const mockKeyStatus: KeyStatus = {
  loaded: true,
  locked: false,
};

const mockGlobalStatus: GlobalStatus = {
  totalFiles: 16,
  encryptedCount: 13,
  uploadedCount: 12,
  pendingCount: 1,
  unencryptedCount: 2,
  storageUsed: "27.4 MB",
};

function App() {
  const [currentSection, setCurrentSection] = useState("Files");
  const [searchQuery, setSearchQuery] = useState("");

  const handleSearch = (query: string) => {
    setSearchQuery(query);
    // Implement search logic here
  };

  const handleAddFiles = () => {
    // Implement file addition logic
  };

  const handleOpenSettings = () => {
    setCurrentSection("Settings");
  };

  const handleFileClick = (file: FileEntry) => {
    // Implement file details view
    console.log("Clicked file:", file);
  };

  return (
    <ThemeProvider defaultTheme="dark" storageKey="blu-ui-theme">
      <div className="min-h-screen bg-background">
        <Header
          keyStatus={mockKeyStatus}
          onSearch={handleSearch}
          onAddFiles={handleAddFiles}
          onOpenSettings={handleOpenSettings}
        />
        
        <div className="flex">
          <Sidebar
            currentSection={currentSection}
            onNavigate={setCurrentSection}
          />
          
          <main className="flex-1 p-6">
            {currentSection === "Files" && (
              <>
                <div className="rounded-lg border bg-card">
                  <FileList
                    files={mockFiles}
                    onFileClick={handleFileClick}
                  />
                </div>
                
                <div className="mt-4 p-4 rounded-lg border bg-card">
                  <p className="text-sm text-muted-foreground">
                    Status: {mockGlobalStatus.totalFiles} files | {mockGlobalStatus.encryptedCount} encrypted | {mockGlobalStatus.uploadedCount} uploaded to S3 | {mockGlobalStatus.pendingCount} pending | {mockGlobalStatus.unencryptedCount} unencrypted
                  </p>
                </div>
              </>
            )}
            
            {currentSection === "Keys" && <KeyManager />}
          </main>
        </div>
        <Toaster />
      </div>
    </ThemeProvider>
  );
}

export default App;