import { useState } from "react";
import { Key, Upload, AlertTriangle } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { useToast } from "@/hooks/use-toast";

export function KeyImport() {
  const [keyData, setKeyData] = useState("");
  const [open, setOpen] = useState(false);
  const { toast } = useToast();

  const handleImport = () => {
    if (!keyData.trim()) {
      toast({
        title: "Invalid key data",
        description: "Please enter the key data to import",
        variant: "destructive"
      });
      return;
    }

    // Here you would validate and import the key
    toast({
      title: "Key imported",
      description: "The encryption key has been imported successfully"
    });
    
    setKeyData("");
    setOpen(false);
  };

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button variant="outline">
          <Key className="mr-2 h-4 w-4" />
          Import Key
        </Button>
      </DialogTrigger>
      <DialogContent className="sm:max-w-[525px]">
        <DialogHeader>
          <DialogTitle>Import Encryption Key</DialogTitle>
          <DialogDescription>
            Paste your encrypted key data below. The key will be stored securely in your local keyring.
          </DialogDescription>
        </DialogHeader>
        
        <Alert variant="warning">
          <AlertTriangle className="h-4 w-4" />
          <AlertTitle>Warning</AlertTitle>
          <AlertDescription>
            Never share your encryption keys or import keys from untrusted sources.
          </AlertDescription>
        </Alert>

        <div className="grid gap-4 py-4">
          <div className="grid gap-2">
            <Label htmlFor="key">Key Data</Label>
            <Textarea
              id="key"
              value={keyData}
              onChange={(e) => setKeyData(e.target.value)}
              placeholder="Paste your encrypted key data here..."
              className="font-mono text-sm"
            />
          </div>
        </div>
        
        <DialogFooter>
          <Button variant="outline" onClick={() => setOpen(false)}>
            Cancel
          </Button>
          <Button onClick={handleImport}>
            <Upload className="mr-2 h-4 w-4" />
            Import
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}