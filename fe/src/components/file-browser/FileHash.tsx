import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";

interface FileHashProps {
  hash: string;
}

export function FileHash({ hash }: FileHashProps) {
  const truncatedHash = `${hash.slice(0, 4)}...${hash.slice(-4)}`;
  
  return (
    <TooltipProvider>
      <Tooltip>
        <TooltipTrigger asChild>
          <span className="cursor-help">{truncatedHash}</span>
        </TooltipTrigger>
        <TooltipContent>
          <p className="font-mono text-xs">{hash}</p>
        </TooltipContent>
      </Tooltip>
    </TooltipProvider>
  );
}