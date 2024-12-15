import { FileEntry } from "@/types";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { FileHash } from "./FileHash";
import { FileStatus } from "./FileStatus";
import { FileTags } from "./FileTags";
import { FilePaths } from "./FilePaths";

interface FileListProps {
  files: FileEntry[];
  onFileClick: (file: FileEntry) => void;
}

export function FileList({ files, onFileClick }: FileListProps) {
  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>Content Hash</TableHead>
          <TableHead>Size</TableHead>
          <TableHead>Status</TableHead>
          <TableHead>Tags</TableHead>
          <TableHead>Paths</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {files.map((file) => (
          <TableRow
            key={file.contentHash}
            className="cursor-pointer hover:bg-muted/50"
            onClick={() => onFileClick(file)}
          >
            <TableCell>
              <FileHash hash={file.contentHash} />
            </TableCell>
            <TableCell>{file.size.toLocaleString()} bytes</TableCell>
            <TableCell>
              <FileStatus status={file.status} />
            </TableCell>
            <TableCell>
              <FileTags tags={file.tags} />
            </TableCell>
            <TableCell>
              <FilePaths paths={file.paths} />
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  );
}