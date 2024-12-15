interface FilePathsProps {
  paths: string[];
}

export function FilePaths({ paths }: FilePathsProps) {
  return (
    <div className="flex flex-col space-y-1">
      {paths.map((path) => (
        <span key={path} className="text-sm text-muted-foreground truncate" title={path}>
          {path}
        </span>
      ))}
    </div>
  );
}