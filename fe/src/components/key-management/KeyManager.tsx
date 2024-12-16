import { useState } from "react";
import { Shield } from "lucide-react";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { KeyStatus } from "./KeyStatus";
import { KeyUnlock } from "./KeyUnlock";
import { KeyImport } from "./KeyImport";
import { useToast } from "@/hooks/use-toast";
import { KeyStatus as KeyStatusType } from "@/types";

export function KeyManager() {
  const [keyStatus, setKeyStatus] = useState<KeyStatusType>({
    loaded: true,
    locked: true
  });
  const { toast } = useToast();

  const handleUnlock = (passphrase: string) => {
    // Simulate key unlocking
    setTimeout(() => {
      setKeyStatus({ loaded: true, locked: false });
      toast({
        title: "Keys unlocked",
        description: "Your encryption keys are now ready to use"
      });
    }, 500);
  };

  return (
    <div className="container mx-auto p-6">
      <div className="flex items-center justify-between mb-6">
        <div className="flex items-center space-x-4">
          <Shield className="h-6 w-6" />
          <h1 className="text-2xl font-bold">Key Management</h1>
        </div>
        <KeyStatus status={keyStatus} />
      </div>

      <div className="grid gap-6">
        <Card>
          <CardHeader>
            <CardTitle>Encryption Keys</CardTitle>
            <CardDescription>
              Manage your encryption keys and security settings
            </CardDescription>
          </CardHeader>
          <CardContent>
            <Tabs defaultValue="unlock" className="w-full">
              <TabsList className="grid w-full grid-cols-2">
                <TabsTrigger value="unlock">Unlock Keys</TabsTrigger>
                <TabsTrigger value="import">Import Keys</TabsTrigger>
              </TabsList>
              <TabsContent value="unlock">
                <KeyUnlock onUnlock={handleUnlock} />
              </TabsContent>
              <TabsContent value="import">
                <KeyImport />
              </TabsContent>
            </Tabs>
          </CardContent>
        </Card>
      </div>
    </div>
  );
}