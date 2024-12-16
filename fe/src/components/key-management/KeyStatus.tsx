import { Lock, Unlock, Shield, ShieldAlert } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";
import { KeyStatus as KeyStatusType } from "@/types";

interface KeyStatusProps {
  status: KeyStatusType;
}

export function KeyStatus({ status }: KeyStatusProps) {
  const { loaded, locked } = status;
  
  return (
    <TooltipProvider>
      <Tooltip>
        <TooltipTrigger asChild>
          <div className="flex items-center space-x-2">
            {loaded ? (
              locked ? (
                <>
                  <Lock className="h-4 w-4 text-yellow-500" />
                  <Badge variant="outline" className="text-yellow-500 border-yellow-500">Locked</Badge>
                </>
              ) : (
                <>
                  <Unlock className="h-4 w-4 text-green-500" />
                  <Badge variant="outline" className="text-green-500 border-green-500">Unlocked</Badge>
                </>
              )
            ) : (
              <>
                <ShieldAlert className="h-4 w-4 text-red-500" />
                <Badge variant="outline" className="text-red-500 border-red-500">No Keys</Badge>
              </>
            )}
          </div>
        </TooltipTrigger>
        <TooltipContent>
          <p>{loaded ? (locked ? "Keys loaded but locked" : "Keys loaded and unlocked") : "No encryption keys loaded"}</p>
        </TooltipContent>
      </Tooltip>
    </TooltipProvider>
  );
}